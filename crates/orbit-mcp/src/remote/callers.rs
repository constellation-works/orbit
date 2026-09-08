//! The destination's statement about who may call it, and as what [ORB-11052].
//!
//! [`CALLERS_FILE`] is the mirror of the federated destinations file:
//! destinations declare who this machine may call, callers declare who may
//! call this machine and with which capabilities. Both are machine-global
//! operator files; neither belongs in a workspace.
//!
//! # Why a destination-side file at all
//!
//! On an SSH destination the caller writes the remote argv, so `--operator` on
//! `orbit mcp serve` was a caller-authored grant: anyone with shell access
//! stamped their own session `Operator` and satisfied every governed
//! operation. This module moves the *statement* to the machine that executes
//! the work — the caller's argv becomes a request, and the file is the
//! ceiling.
//!
//! # How strong the identity is depends on the destination
//!
//! Two tiers share this file, and a reader must never assume which one
//! answered. Under Tier 1 the caller identity is self-asserted:
//! `--remote-caller-machine-id` is a label the caller chooses, so a caller that
//! can reach this destination can also name a different row. That is an
//! accident guard, in keeping with the governance kernel's doctrine — strictly
//! stronger than a caller-authored grant, and not an authenticated boundary.
//! The only trusted-host exception is a destination owner's explicit
//! operation-specific `cooperative` mode, which accepts that limitation for
//! operators already sharing the destination OS account and records it
//! candidly.
//!
//! Under Tier 2 [ORB-11053] the destination pins the identity to a key in its
//! own `authorized_keys`, sshd authenticates that key, and the forced command
//! Orbit runs names the caller. There the identity is a real boundary for the
//! remote case. [`RemoteCallerIdentity`] carries which of the two applies all
//! the way into the audit row, so the difference is recorded rather than
//! assumed. See [`super::ssh_auth`].

use std::collections::{BTreeSet, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::protocol::toml::escape_basic_string;
use orbit_types::identity::validate_machine_id;
use orbit_types::tool::{
    CallerIdentityProof, McpCapability, RemoteAgentInvokeMode, RemoteCallerGrant,
};
use serde::Deserialize;

use super::identity::McpSessionAuthority;
use super::ssh_auth::{self, ObservedKeys};

pub const CALLERS_FILE: &str = "mcp-callers.toml";

/// How the file is named in operator-facing text.
///
/// Denials and warnings quote the path the operator would type, not the
/// expanded home directory of whichever account the destination runs as.
pub const CALLERS_FILE_DISPLAY: &str = "~/.orbit/mcp-callers.toml";

/// What a destination serves a caller that matches no row.
///
/// Operator is deliberately absent: a default that could grant it would make
/// the escalation this file exists to close reachable by omitting a row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultGrant {
    #[default]
    Agent,
    Deny,
}

impl DefaultGrant {
    fn capabilities(self) -> BTreeSet<McpCapability> {
        match self {
            Self::Agent => BTreeSet::from([McpCapability::Agent]),
            Self::Deny => BTreeSet::new(),
        }
    }
}

/// One caller this destination has agreed to serve.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerRow {
    /// The calling machine's stable `hm_…` identity.
    pub machine_id: String,
    /// The ceiling this destination serves that caller. `agent` and
    /// `operator` only; `runner` is stamped in-process by a managed run and
    /// can never arrive over a transport.
    pub capabilities: Vec<String>,
    /// Operator-facing display name. Never an identity input.
    #[serde(default)]
    pub label: Option<String>,
    /// Narrows the grant to these logical `ws_*` IDs.
    #[serde(default)]
    pub workspaces: Option<Vec<String>>,
    /// Binds the row to a key sshd authenticated, in the `SHA256:…` form
    /// `ssh-keygen -l` prints [ORB-11053]. Enforced at session establishment
    /// whenever the destination can observe the authenticating key; a
    /// destination that cannot observe it serves the session and records that
    /// the identity was unverified, because a fingerprint nothing can check is
    /// not evidence of a mismatch.
    #[serde(default)]
    pub ssh_key_fingerprint: Option<String>,
    /// Explicitly admits `orbit.agent.invoke` for this caller on its
    /// operation-specific workspace scope. When the independent scope is
    /// absent, `workspaces` remains the legacy invocation scope.
    #[serde(default)]
    pub agent_invoke: bool,
    /// Narrows `agent_invoke` without narrowing ordinary capabilities.
    #[serde(default)]
    pub agent_invoke_workspaces: Option<Vec<String>>,
    /// Trust model for `agent_invoke`. Omission preserves strict key-bound
    /// admission; `cooperative` deliberately accepts the self-asserted caller
    /// label on the existing SSH operator channel.
    #[serde(default)]
    pub agent_invoke_mode: Option<RemoteAgentInvokeMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallersFile {
    #[serde(default)]
    pub default: DefaultGrant,
    #[serde(default)]
    pub callers: Vec<CallerRow>,
}

pub fn callers_path(global_orbit_root: &Path) -> PathBuf {
    global_orbit_root.join(CALLERS_FILE)
}

/// Load and validate the callers file.
///
/// A missing file is valid and means `default = "agent"` with no rows. A
/// malformed one is never served as if absent: it fails the whole file closed
/// here, at load, before any session is served.
pub fn load_callers(path: &Path) -> Result<CallersFile, OrbitError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CallersFile::default());
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to read MCP callers '{}': {error}",
                path.display()
            )));
        }
    };
    let file: CallersFile = toml::from_str(&contents).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid MCP callers '{}': {error}", path.display()))
    })?;
    validate_callers(&file, path)?;
    Ok(file)
}

fn validate_callers(file: &CallersFile, path: &Path) -> Result<(), OrbitError> {
    let mut seen = HashSet::with_capacity(file.callers.len());
    for row in &file.callers {
        if !seen.insert(row.machine_id.as_str()) {
            return Err(OrbitError::AmbiguousCaller(format!(
                "machine_id '{}' appears more than once in '{}'",
                row.machine_id,
                path.display()
            )));
        }
    }
    for row in &file.callers {
        validate_machine_id(&row.machine_id).map_err(|error| {
            invalid(
                path,
                format!("invalid machine_id '{}': {error}", row.machine_id),
            )
        })?;
        parse_capabilities(row, path)?;
        validate_workspace_scope(row, row.workspaces.as_deref(), "workspaces", path)?;
        validate_workspace_scope(
            row,
            row.agent_invoke_workspaces.as_deref(),
            "agent_invoke_workspaces",
            path,
        )?;
        if let Some(defect) = row
            .ssh_key_fingerprint
            .as_deref()
            .and_then(ssh_auth::fingerprint_defect)
        {
            return Err(invalid(
                path,
                format!(
                    "caller '{}' pins a key fingerprint that {defect}",
                    row.machine_id
                ),
            ));
        }
        if row.agent_invoke {
            if !row.capabilities.iter().any(|value| value == "operator") {
                return Err(invalid(
                    path,
                    format!(
                        "caller '{}' enables `agent_invoke` without the `operator` capability",
                        row.machine_id
                    ),
                ));
            }
            if row.workspaces.is_none() && row.agent_invoke_workspaces.is_none() {
                return Err(invalid(
                    path,
                    format!(
                        "caller '{}' enables `agent_invoke` without a workspace scope; set \
                         `agent_invoke_workspaces` or the legacy `workspaces` narrowing",
                        row.machine_id
                    ),
                ));
            }
            if row.agent_invoke_mode.unwrap_or_default() == RemoteAgentInvokeMode::KeyBound
                && row.ssh_key_fingerprint.is_none()
            {
                return Err(invalid(
                    path,
                    format!(
                        "caller '{}' enables `agent_invoke` without `ssh_key_fingerprint`; \
                         remote trusted-host execution requires a key-bound caller identity",
                        row.machine_id
                    ),
                ));
            }
        } else if row.agent_invoke_mode.is_some() || row.agent_invoke_workspaces.is_some() {
            return Err(invalid(
                path,
                format!(
                    "caller '{}' sets an agent-invocation option without enabling `agent_invoke`; \
                     those options only qualify that operation-specific grant",
                    row.machine_id
                ),
            ));
        }
    }
    Ok(())
}

fn validate_workspace_scope(
    row: &CallerRow,
    workspaces: Option<&[String]>,
    field: &str,
    path: &Path,
) -> Result<(), OrbitError> {
    let Some(workspaces) = workspaces else {
        return Ok(());
    };
    if workspaces.is_empty() {
        return Err(invalid(
            path,
            format!(
                "caller '{}' has an empty `{field}` list; omit the key rather than granting no \
                 workspaces",
                row.machine_id
            ),
        ));
    }
    for workspace in workspaces {
        if !workspace.starts_with("ws_") {
            return Err(invalid(
                path,
                format!(
                    "caller '{}' narrows `{field}` to '{workspace}', which is not a logical \
                     workspace ID; workspace scopes take `ws_*` IDs",
                    row.machine_id
                ),
            ));
        }
    }
    Ok(())
}

/// The row's capabilities, as the grantable subset of the capability
/// vocabulary.
///
/// `runner` parses as a capability everywhere else in Orbit, which is exactly
/// why it is rejected by name here rather than left to fall through an unknown
/// value: a run's own sanction must not be reachable over a transport.
fn parse_capabilities(row: &CallerRow, path: &Path) -> Result<BTreeSet<McpCapability>, OrbitError> {
    if row.capabilities.is_empty() {
        return Err(invalid(
            path,
            format!(
                "caller '{}' has an empty `capabilities` list; use `default = \"deny\"` or remove \
                 the row",
                row.machine_id
            ),
        ));
    }
    row.capabilities
        .iter()
        .map(|capability| match capability.as_str() {
            "agent" => Ok(McpCapability::Agent),
            "operator" => Ok(McpCapability::Operator),
            other => Err(invalid(
                path,
                format!(
                    "caller '{}' declares capability '{other}'; only `agent` and `operator` may \
                     be granted to a caller",
                    row.machine_id
                ),
            )),
        })
        .collect()
}

fn invalid(path: &Path, detail: String) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "invalid MCP callers '{}': {detail}",
        path.display()
    ))
}

/// The caller identity a destination resolves a grant for, and how strongly it
/// knows it [ORB-11053].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCallerIdentity {
    /// The `hm_…` identity a row is selected by.
    pub machine_id: String,
    /// Whether the destination composed that identity itself, next to a key
    /// sshd authenticated, or the caller merely claimed it.
    pub proof: CallerIdentityProof,
    /// The keys sshd accepted, when this destination can see them. `None`
    /// means verification is unavailable here, which is not a mismatch.
    pub observed_keys: Option<ObservedKeys>,
}

impl RemoteCallerIdentity {
    /// An identity the caller claimed. Selects a row and proves nothing.
    pub fn self_asserted(machine_id: impl Into<String>) -> Self {
        Self {
            machine_id: machine_id.into(),
            proof: CallerIdentityProof::SelfAsserted,
            observed_keys: None,
        }
    }

    /// An identity this destination wrote next to a key in its own
    /// `authorized_keys`, which sshd authenticated before running the forced
    /// command that carries it.
    pub fn key_bound(machine_id: impl Into<String>, observed_keys: Option<ObservedKeys>) -> Self {
        Self {
            machine_id: machine_id.into(),
            proof: CallerIdentityProof::KeyBound,
            observed_keys,
        }
    }

    /// Attach the keys sshd accepted to an already-resolved identity.
    ///
    /// A pinned row is enforced under either tier: the operator wrote the
    /// fingerprint to have it checked, and a Tier 1 destination that happens to
    /// run `ExposeAuthInfo` can check it just as well. What Tier 2 adds is that
    /// the *identity itself* is no longer the caller's to choose.
    pub fn observing(mut self, observed_keys: Option<ObservedKeys>) -> Self {
        self.observed_keys = observed_keys;
        self
    }
}

/// What this destination will serve one caller, before the caller's request is
/// taken into account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCallerGrant {
    /// The caller identity this grant was resolved for.
    pub caller_machine_id: String,
    /// How that identity was established.
    pub identity: CallerIdentityProof,
    /// The key the matched row pins the caller to, if it pins one.
    pub pinned_fingerprint: Option<String>,
    /// The row's `label`, when one matched.
    pub label: Option<String>,
    /// Capabilities on the workspaces this grant covers.
    pub granted: BTreeSet<McpCapability>,
    /// Capabilities anywhere the grant does not cover — the file default.
    pub elsewhere: BTreeSet<McpCapability>,
    /// The `ws_*` IDs [`Self::granted`] applies to. `None` means every
    /// workspace on this destination.
    pub workspaces: Option<BTreeSet<String>>,
    /// Whether the matched row explicitly admits trusted-host agent invocation.
    pub agent_invoke: bool,
    /// The `ws_*` IDs the explicit agent-invocation grant applies to.
    pub agent_invoke_workspaces: Option<BTreeSet<String>>,
    /// Trust mode selected for agent invocation, when it is enabled.
    pub agent_invoke_mode: Option<RemoteAgentInvokeMode>,
    /// Whether a row matched, or the file default answered.
    pub matched: bool,
}

impl ResolvedCallerGrant {
    /// The grant that applies to a call landing in `workspace_id`.
    ///
    /// A narrowed row falls back to the file default outside its listed
    /// workspaces rather than to a fixed `agent`, so `default = "deny"` still
    /// denies there. `None` — a call that resolved no workspace — takes the
    /// unnarrowed grant: every governed operation is workspace-scoped, so a
    /// workspace-less call is a discovery call the narrowing has nothing to
    /// say about.
    pub fn for_workspace(&self, workspace_id: Option<&str>) -> BTreeSet<McpCapability> {
        match (&self.workspaces, workspace_id) {
            (Some(covered), Some(workspace_id)) if !covered.contains(workspace_id) => {
                self.elsewhere.clone()
            }
            _ => self.granted.clone(),
        }
    }

    /// Whether the explicit agent-invocation grant covers `workspace_id`.
    pub fn agent_invoke_for_workspace(&self, workspace_id: Option<&str>) -> bool {
        self.agent_invoke
            && match (&self.agent_invoke_workspaces, workspace_id) {
                (Some(covered), Some(workspace_id)) => covered.contains(workspace_id),
                (Some(_), None) | (None, _) => false,
            }
    }

    /// The selected trust mode when the operation grant covers `workspace_id`.
    pub fn agent_invoke_mode_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Option<RemoteAgentInvokeMode> {
        self.agent_invoke_for_workspace(workspace_id)
            .then_some(self.agent_invoke_mode.unwrap_or_default())
    }
}

impl CallersFile {
    /// What this destination serves `identity`.
    ///
    /// An absent, malformed, or unmatched caller identity falls to the file
    /// default. It never falls back to the caller's argv: that is the
    /// escalation being closed.
    pub fn resolve(&self, identity: &RemoteCallerIdentity) -> ResolvedCallerGrant {
        let caller_machine_id = identity.machine_id.as_str();
        let default = self.default.capabilities();
        let Some(row) = self
            .callers
            .iter()
            .find(|row| row.machine_id == caller_machine_id)
        else {
            return ResolvedCallerGrant {
                caller_machine_id: caller_machine_id.to_string(),
                identity: identity.proof,
                pinned_fingerprint: None,
                label: None,
                granted: default.clone(),
                elsewhere: default,
                workspaces: None,
                agent_invoke: false,
                agent_invoke_workspaces: None,
                agent_invoke_mode: None,
                matched: false,
            };
        };
        // Validated at load, so an unparseable capability here cannot reach a
        // served session; treat it as the default rather than panicking.
        let granted = row
            .capabilities
            .iter()
            .filter_map(|capability| capability.parse::<McpCapability>().ok())
            .filter(|capability| *capability != McpCapability::Runner)
            .collect::<BTreeSet<_>>();
        ResolvedCallerGrant {
            caller_machine_id: caller_machine_id.to_string(),
            identity: identity.proof,
            pinned_fingerprint: row.ssh_key_fingerprint.clone(),
            label: row.label.clone(),
            granted,
            elsewhere: default,
            workspaces: row
                .workspaces
                .as_ref()
                .map(|workspaces| workspaces.iter().cloned().collect()),
            agent_invoke: row.agent_invoke,
            agent_invoke_workspaces: row
                .agent_invoke_workspaces
                .as_ref()
                .or(row.workspaces.as_ref())
                .map(|workspaces| workspaces.iter().cloned().collect()),
            agent_invoke_mode: row
                .agent_invoke
                .then_some(row.agent_invoke_mode.unwrap_or_default()),
            matched: true,
        }
    }
}

/// Whether this server process was started by sshd for a non-interactive
/// session.
///
/// Both halves are load-bearing. `SSH_CONNECTION` is set by sshd in the server
/// process, so a caller can neither forge it nor — the part that matters —
/// omit it; keying on `--remote-caller-machine-id` instead would let a caller
/// present a remote session as a local one by dropping the flag. The
/// non-terminal check separates the MCP transport, whose argv is `ssh -T`,
/// from a person who SSH'd in and started a server by hand.
pub fn remote_originated() -> bool {
    use std::io::IsTerminal;
    std::env::var("SSH_CONNECTION").is_ok_and(|value| !value.trim().is_empty())
        && !std::io::stdin().is_terminal()
}

/// The capabilities one MCP session may hold, and where they came from.
///
/// Built once at session establishment. A local session carries no grant and
/// keeps today's argv authority byte for byte; a remote-originated one carries
/// the destination's grant and is capped by it on every call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCapabilityPolicy {
    requested: BTreeSet<McpCapability>,
    grant: Option<ResolvedCallerGrant>,
}

impl SessionCapabilityPolicy {
    /// A session whose authority is the process's own argv.
    ///
    /// This is every non-remote-originated stdio session, and also the two
    /// surfaces whose authority is not a caller's to ask for at all: the TCP
    /// listener, which authenticates nobody and is hardcoded to `agent`, and
    /// the federated mux's client side, which is a caller here rather than a
    /// destination.
    pub fn local(authority: McpSessionAuthority) -> Self {
        Self {
            requested: authority.capabilities(),
            grant: None,
        }
    }

    /// A session capped by an already-resolved grant.
    ///
    /// `orbit mcp callers check` answers what a caller *would* get without
    /// serving a session, and must compute it the same way a served session
    /// does rather than restating the intersection.
    pub fn from_grant(authority: McpSessionAuthority, grant: ResolvedCallerGrant) -> Self {
        Self {
            requested: authority.capabilities(),
            grant: Some(grant),
        }
    }

    /// Resolve a stdio `orbit mcp serve` session against this destination.
    ///
    /// The caller's argv supplies the request; the callers file supplies the
    /// ceiling; the session gets the intersection. Because it is an
    /// intersection, the file can only lower a session — a caller granted
    /// `operator` that did not pass `--operator` still resolves to `agent`,
    /// so this opens no privilege path that argv alone did not already have.
    pub fn resolve(
        global_root: &Path,
        authority: McpSessionAuthority,
        identity: &RemoteCallerIdentity,
    ) -> Result<Self, OrbitError> {
        let path = callers_path(global_root);
        let exists = path.exists();
        let file = load_callers(&path)?;
        if !exists {
            // The migration is a downgrade and will cut operator-over-SSH
            // flows on first upgrade. That is the intended direction, so it is
            // announced rather than silently applied.
            tracing::warn!(
                target: "orbit.mcp.callers",
                path = %path.display(),
                "no MCP callers file on this destination; remote sessions are served agent \
                 capabilities only — run `orbit mcp callers init` to declare callers"
            );
        }
        let grant = file.resolve(identity);
        enforce_key_binding(&grant, identity)?;
        Ok(Self {
            requested: authority.capabilities(),
            grant: Some(grant),
        })
    }

    /// Whether the destination's callers file governs this session.
    pub fn is_granted(&self) -> bool {
        self.grant.is_some()
    }

    /// The caller identity this destination resolved the grant for.
    ///
    /// This is the identity the audit envelope must carry, and it is not
    /// always the label the caller forwarded: under a forced command it is
    /// what the destination itself wrote next to the authenticating key.
    pub fn caller_machine_id(&self) -> Option<&str> {
        self.grant
            .as_ref()
            .map(|grant| grant.caller_machine_id.as_str())
    }

    /// How the caller identity behind this session was established.
    pub fn caller_identity(&self) -> Option<CallerIdentityProof> {
        self.grant.as_ref().map(|grant| grant.identity)
    }

    /// Effective capabilities for a call landing in `workspace_id`.
    pub fn effective_for(&self, workspace_id: Option<&str>) -> BTreeSet<McpCapability> {
        let Some(grant) = &self.grant else {
            return self.requested.clone();
        };
        let granted = grant.for_workspace(workspace_id);
        self.requested
            .intersection(&granted)
            .copied()
            .collect::<BTreeSet<_>>()
    }

    /// Effective capabilities anywhere a `workspaces` narrowing does not
    /// cover, so `orbit mcp callers check` can answer the per-workspace
    /// question a narrowed row actually poses instead of naming one scope.
    pub fn effective_outside_narrowing(&self) -> BTreeSet<McpCapability> {
        let Some(grant) = &self.grant else {
            return self.requested.clone();
        };
        self.requested
            .intersection(&grant.elsewhere)
            .copied()
            .collect()
    }

    /// The grant to record alongside the effective set for a call landing in
    /// `workspace_id`. `None` for a local session, which has no grant to
    /// distinguish from its own stamp.
    pub fn grant_for(&self, workspace_id: Option<&str>) -> Option<RemoteCallerGrant> {
        let grant = self.grant.as_ref()?;
        Some(RemoteCallerGrant {
            caller_machine_id: grant.caller_machine_id.clone(),
            granted_capabilities: grant.for_workspace(workspace_id),
            source: CALLERS_FILE_DISPLAY.to_string(),
            identity: grant.identity,
            agent_invoke: grant.agent_invoke_for_workspace(workspace_id),
            agent_invoke_mode: grant.agent_invoke_mode_for_workspace(workspace_id),
        })
    }

    /// Stamp `context` with the capabilities and grant for a call landing in
    /// `workspace_id`.
    ///
    /// Called once at session establishment with the session's workspace, and
    /// again per call once the destination has resolved which registered
    /// workspace the call actually lands in — that resolution is the only
    /// point at which a `workspaces` narrowing can be evaluated.
    pub fn stamp(
        &self,
        context: &mut orbit_types::tool::ToolSessionContext,
        workspace_id: Option<&str>,
    ) {
        context.effective_capabilities = self.effective_for(workspace_id);
        context.remote_caller_grant = self.grant_for(workspace_id);
    }
}

/// Refuse a session whose authenticating key is not the one its row pins.
///
/// The refusal is at session establishment and it is a refusal, not a
/// downgrade: serving the caller at the file default would make a key mismatch
/// — which is either a misconfiguration or somebody else's key — look exactly
/// like a caller that legitimately holds a smaller grant, and the operator who
/// wrote the fingerprint would never learn the difference.
///
/// An unobservable key is a different situation and is deliberately not a
/// refusal. `ExposeAuthInfo` is off in a stock `sshd_config`, and there is no
/// evidence of a mismatch in the absence of evidence; the session is served
/// and the gap is announced once, where an operator will see it.
///
/// Observing that *no* key authenticated is evidence, not the absence of it. A
/// password or keyboard-interactive login under a pinned row is the plainest
/// mismatch that row can have, and it is refused like any other.
fn enforce_key_binding(
    grant: &ResolvedCallerGrant,
    identity: &RemoteCallerIdentity,
) -> Result<(), OrbitError> {
    let Some(pinned) = &grant.pinned_fingerprint else {
        return Ok(());
    };
    let Some(observed) = &identity.observed_keys else {
        tracing::warn!(
            target: "orbit.mcp.callers",
            caller_machine_id = %identity.machine_id,
            identity = %identity.proof,
            "caller row pins an SSH key but this destination cannot observe the authenticating \
             key; set `ExposeAuthInfo yes` in sshd_config, or supply the fingerprint from an \
             AuthorizedKeysCommand, to have the pin enforced"
        );
        return Ok(());
    };
    if observed.matches(pinned) {
        return Ok(());
    }

    let source = observed.observation.label();
    let mismatch = if observed.fingerprints.is_empty() {
        format!("this session authenticated without a public key (seen through {source})")
    } else {
        format!(
            "the key that authenticated this session is {keys} (seen through {source})",
            keys = observed.label(),
        )
    };

    Err(OrbitError::UnauthorizedCaller(format!(
        "caller '{caller}' is pinned to {pinned} by {CALLERS_FILE_DISPLAY} on this machine, but \
         {mismatch}",
        caller = identity.machine_id,
    )))
}

/// What this machine's caller authorization looks like from the outside, for
/// `orbit doctor` [ORB-11053].
///
/// Facts only. The severity of each one, and how it is worded, is the
/// diagnosing surface's business — this crate speaks MCP and owns the file, not
/// the doctor's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerAuthorizationHealth {
    /// Where the callers file would be.
    pub path: PathBuf,
    /// Whether it is there.
    pub present: bool,
    /// Why it does not load, when it is there and does not.
    pub defect: Option<String>,
    /// Whether this machine accepts SSH logins at all, and therefore whether a
    /// missing callers file is a live gap or a fact about a machine nobody
    /// calls.
    pub serves_ssh: bool,
    /// Callers granted `operator` with no `ssh_key_fingerprint`: the grant
    /// that most wants a key behind it, resting on a name the caller chose.
    pub unpinned_operator_callers: Vec<String>,
    /// Rows in the file, for a summary line.
    pub row_count: usize,
}

/// Inspect this machine's caller authorization without serving a session.
///
/// `authorized_keys` is the evidence that this machine is reachable over SSH at
/// all. It is a weaker signal than a fleet registry would be and is not used
/// for any decision — only to keep `orbit doctor` from nagging a laptop that
/// serves nobody about a file it has no reason to write.
pub fn inspect_caller_authorization(
    global_root: &Path,
    authorized_keys: &Path,
) -> CallerAuthorizationHealth {
    let path = callers_path(global_root);
    let present = path.exists();
    let serves_ssh = std::fs::read_to_string(authorized_keys).is_ok_and(|contents| {
        contents
            .lines()
            .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    });
    match load_callers(&path) {
        Ok(file) => CallerAuthorizationHealth {
            path,
            present,
            defect: None,
            serves_ssh,
            unpinned_operator_callers: file
                .callers
                .iter()
                .filter(|row| {
                    row.ssh_key_fingerprint.is_none()
                        && row.capabilities.iter().any(|value| value == "operator")
                })
                .map(|row| row.machine_id.clone())
                .collect(),
            row_count: file.callers.len(),
        },
        Err(error) => CallerAuthorizationHealth {
            path,
            present,
            defect: Some(error.to_string()),
            serves_ssh,
            unpinned_operator_callers: Vec::new(),
            row_count: 0,
        },
    }
}

/// One caller `orbit mcp callers init` seeds a row for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SeedCaller {
    pub machine_id: String,
    pub label: Option<String>,
}

/// Render a seed callers file granting every named caller `agent`.
///
/// `operator` is never written. The seeder exists to save an operator from
/// transcribing machine IDs, not to decide who may dispatch a workflow on this
/// machine — that has to stay a deliberate edit, or the file would re-create
/// the caller-authored grant it replaces.
pub fn render_callers_seed(callers: &[SeedCaller]) -> String {
    let mut out = String::from(
        "# Which callers this machine serves, and with what capabilities.\n\
         # Seeded by `orbit mcp callers init`, which grants agent only.\n\
         # Raising a row to operator is a deliberate hand edit.\n\
         \n\
         # Capabilities served to a caller that matches no row below.\n\
         # Permitted values: \"agent\" or \"deny\".\n\
         default = \"agent\"\n",
    );
    for caller in callers {
        out.push_str("\n[[callers]]\n");
        out.push_str(&format!("machine_id   = \"{}\"\n", caller.machine_id));
        if let Some(label) = &caller.label {
            // A label comes from a peer's operator-chosen host id or an SSH
            // destination string; a quote in it must not end the literal.
            out.push_str(&format!(
                "label        = \"{}\"\n",
                escape_basic_string(label)
            ));
        }
        out.push_str("capabilities = [\"agent\"]\n");
    }
    out
}

/// Write a seed callers file, refusing to overwrite an existing one.
///
/// An existing file is an operator's statement about who may do what here;
/// re-running the seeder must never silently revoke an `operator` grant it is
/// forbidden to write back.
pub fn write_callers_seed(path: &Path, contents: &str) -> Result<(), OrbitError> {
    if path.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "MCP callers file '{}' already exists; edit it directly rather than re-seeding",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrbitError::Io(format!("failed to create '{}': {error}", parent.display()))
        })?;
    }
    std::fs::write(path, contents).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write MCP callers '{}': {error}",
            path.display()
        ))
    })
}
