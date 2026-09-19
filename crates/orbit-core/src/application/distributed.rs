//! Owner-side read-only surface of the distributed drain: the admission
//! preflight probe ([design §4.1]) and receipt reconciliation ([spec]).
//!
//! # Authority
//!
//! SSH login establishes owner access [ORB-12564]. There is no destination
//! callers file, forced-command acceptance, key-bound proof, or replacement
//! identity registry. What still decides a call here is the session the
//! destination serves — its `agent`/`operator` capabilities, resolved by the
//! tool chokepoint — plus the trusted runtime facts this module reads. A
//! machine label forwarded by an SSH proxy or the federated mux is attribution:
//! it names a receipt namespace and appears in diagnostics, and it never adds a
//! capability the session did not already hold.
//!
//! # What this module does not do
//!
//! Nothing here creates an admission receipt, a claim, a reservation, or a task
//! transition, and nothing here grants execution authority. The mutating
//! distributed entry points — pull, run binding, settlement, handoff,
//! completion approval — stay unavailable behind
//! [`ensure_distributed_mutation_available`] until the lifecycle integration
//! slice wires trusted claim context through the routed transport.
//!
//! [design §4.1]: ../../../../docs/design/distributed-drain/2_design.md
//! [spec]: ../../../../docs/design/distributed-drain/specs/task-pull.md

use orbit_common::OrbitError;
use orbit_store::contracts::{
    ADMISSION_RECEIPT_LOOKUP_SCHEMA, AdmissionIdentity, AdmissionLookup, AdmissionRefusal,
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, ExecutionLocation,
};
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use serde::Serialize;

/// Whether the mutating distributed entry points are reachable from any public
/// surface.
///
/// It is a constant rather than configuration on purpose: an incomplete feature
/// must not become reachable because an operator set a key or an environment
/// variable. Flipping it is a deliberate source change made by the slice that
/// lands routed mutations, and every gated entry point names it.
pub const DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED: bool = false;

/// Refuse a mutating distributed entry point while the feature is incomplete.
pub fn ensure_distributed_mutation_available(entry_point: &str) -> Result<(), OrbitError> {
    if DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED {
        return Ok(());
    }
    Err(OrbitError::CapabilityRefused(format!(
        "distributed entry point '{entry_point}' is unavailable: claim binding, routed \
         mutations, settlement, and handoff integration have not landed, so the owner serves \
         only the read-only preflight and receipt lookup"
    )))
}

/// What the owner reports about itself before a follower enables pull.
///
/// Read-only by construction: every field is derived from workspace
/// configuration, the running binary, and the calling session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainProbeReport {
    /// Response shape version of the probe itself.
    pub schema_version: u32,
    /// Distributed-drain wire-protocol version this owner speaks.
    pub protocol_schema: u32,
    pub binary_version: String,
    pub workspace_id: String,
    /// Registered owner machine, absent on a standalone registry that predates
    /// host identity.
    pub owner_machine_id: Option<String>,
    /// Session facts, for diagnostics. Capabilities are what the destination
    /// serves this session; the caller machine is attribution only.
    pub session: DrainProbeSession,
    /// Ship configuration the owner would resolve at admission.
    pub ship: AdmissionShipContract,
    /// Whether the owner's own review policy is the only admissible one.
    pub review_policy: String,
    /// `true` when the declared caller contract would pass the admission
    /// ladder now. Absent declarations leave it `true` with no refusal: the
    /// probe reports what it was asked about and pull rechecks everything.
    pub admits: bool,
    /// First refusal the shared admission ladder reports, by spec name.
    pub refusal: Option<String>,
    /// Human-readable detail for each mismatch the probe observed.
    pub diagnostics: Vec<String>,
    /// Pinned `false`: a probe is never an admission.
    pub creates_admission_state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainProbeSession {
    pub capabilities: Vec<String>,
    /// Forwarded caller label, or this machine for a local session. Diagnostic.
    pub caller_machine_id: Option<String>,
    pub remote: bool,
}

/// What the caller declares about itself, so the probe can answer the question
/// a follower actually has: *would my pull be admitted right now?*
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredCallerContract {
    pub caller_version: Option<String>,
    pub caller_schema: Option<u32>,
    pub caller_review_policy: Option<String>,
}

impl DeclaredCallerContract {
    fn is_empty(&self) -> bool {
        self.caller_version.is_none()
            && self.caller_schema.is_none()
            && self.caller_review_policy.is_none()
    }
}

/// Outcome of one read-only receipt reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdmissionReceiptLookup {
    /// Lookup schema, versioned independently of admission.
    pub lookup_schema: u32,
    pub request_id: String,
    /// Receipt namespace the lookup read: the original caller machine.
    pub machine_id: String,
    /// `found`, `expired`, or `not_found`.
    pub outcome: String,
    /// The original receipt, byte-for-byte as admission stored it. The input it
    /// records is never rewritten to match the caller's current binary.
    pub receipt: Option<serde_json::Value>,
    /// Current claim phase, separate from the stored receipt.
    pub current_claim: Option<serde_json::Value>,
    /// Pinned `false`: a receipt is historical evidence, not authority.
    pub grants_execution_authority: bool,
    /// Pinned `false`: `not_found` is not proof that an earlier transport
    /// request cannot still arrive, so it never licenses a replacement request
    /// under a new ID.
    pub permits_replacement_request: bool,
    pub guidance: String,
}

impl crate::OrbitRuntime {
    /// Serve the read-only admission probe for this workspace.
    ///
    /// Refusal order matches the spec ladder: the selector is resolved by the
    /// surface that opened this runtime, then destination authority (a replica
    /// serves no control-plane work), then session capability, then the
    /// store-owned shape/version/mode/policy ladder — which the probe reports
    /// rather than raising, because observing a mismatch before admission is
    /// the whole point of a preflight.
    pub fn drain_probe(
        &self,
        session: &ToolSessionContext,
        declared: &DeclaredCallerContract,
    ) -> Result<DrainProbeReport, OrbitError> {
        self.ensure_distributed_owner_workspace()?;
        ensure_session_agent_capability(session)?;
        let ship = self.owner_ship_contract();
        let mut diagnostics = Vec::new();
        let refusal = self.declared_contract_refusal(session, declared, &ship, &mut diagnostics)?;
        Ok(DrainProbeReport {
            schema_version: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            protocol_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            binary_version: owner_binary_version().to_string(),
            workspace_id: self.workspace_id()?,
            owner_machine_id: self.distributed_owner_machine_id(),
            session: probe_session(session),
            review_policy: ship.review_policy.clone(),
            ship,
            admits: refusal.is_none(),
            refusal: refusal.map(|refusal| refusal.as_str().to_string()),
            diagnostics,
            creates_admission_state: false,
        })
    }

    /// Reconcile one admission request identity without touching admission
    /// state.
    ///
    /// `requested_machine` names the receipt namespace. A worker session reads
    /// its own namespace: the trusted session machine, never a machine named in
    /// tool input. An operator session may inspect across attempts, which is
    /// the cross-attempt access the design keeps for deliberate recovery — not
    /// a revival of the retired cross-caller ACL, because it is decided by the
    /// capability the destination serves this session.
    pub fn lookup_admission_receipt(
        &self,
        session: &ToolSessionContext,
        request_id: &str,
        requested_machine: Option<&str>,
        lookup_schema: u32,
    ) -> Result<AdmissionReceiptLookup, OrbitError> {
        self.ensure_distributed_owner_workspace()?;
        ensure_session_agent_capability(session)?;
        if lookup_schema != ADMISSION_RECEIPT_LOOKUP_SCHEMA {
            // An incompatible lookup protocol refuses; the remedy is owner
            // claim inspection and deliberate recovery, never a replacement
            // request under a new ID.
            return Err(OrbitError::InvalidInput(format!(
                "version_mismatch: this owner serves receipt lookup schema \
                 {ADMISSION_RECEIPT_LOOKUP_SCHEMA}, not {lookup_schema}; reconcile through owner \
                 claim inspection instead"
            )));
        }
        let request_id = request_id.trim();
        if request_id.is_empty() {
            return Err(OrbitError::InvalidInput(
                "invalid_input: `request_id` is required".into(),
            ));
        }
        let session_machine = session_machine_id(session);
        let machine_id = match requested_machine.map(str::trim).filter(|m| !m.is_empty()) {
            None => session_machine.clone().ok_or_else(|| {
                OrbitError::InvalidInput(
                    "invalid_input: no trusted caller machine on this session; name the original \
                     caller machine with `machine_id` from an operator session"
                        .into(),
                )
            })?,
            Some(requested) => {
                if !session.has_capability(McpCapability::Operator)
                    && session_machine.as_deref() != Some(requested)
                {
                    return Err(OrbitError::CapabilityRefused(
                        "cross-attempt receipt inspection requires operator capability; a worker \
                         session reads its own receipt namespace"
                            .into(),
                    ));
                }
                requested.to_string()
            }
        };
        let identity = trusted_identity(&machine_id, session);
        let lookup = self
            .stores()
            .tasks()
            .lookup_admission(&identity, request_id)?;
        let (outcome, receipt, current_claim) = match lookup {
            AdmissionLookup::Found {
                receipt,
                current_claim,
            } => (
                "found",
                Some(serde_json::to_value(&*receipt).map_err(json_error)?),
                current_claim
                    .map(|claim| serde_json::to_value(&*claim))
                    .transpose()
                    .map_err(json_error)?,
            ),
            AdmissionLookup::Expired => ("expired", None, None),
            AdmissionLookup::NotFound => ("not_found", None, None),
        };
        Ok(AdmissionReceiptLookup {
            lookup_schema: ADMISSION_RECEIPT_LOOKUP_SCHEMA,
            request_id: request_id.to_string(),
            machine_id,
            outcome: outcome.to_string(),
            receipt,
            current_claim,
            grants_execution_authority: false,
            permits_replacement_request: false,
            guidance: lookup_guidance(outcome).to_string(),
        })
    }

    /// Owner-side claim listing for inspection and deliberate recovery.
    ///
    /// Age, reservation expiry, and an absent local run are diagnostics, not
    /// proof of death: nothing here reclaims, rebinds, or repairs.
    pub fn inspect_distributed_claims(&self) -> Result<Vec<serde_json::Value>, OrbitError> {
        self.ensure_distributed_owner_workspace()?;
        self.inspect_execution_claims()?
            .iter()
            .map(|claim| serde_json::to_value(claim).map_err(json_error))
            .collect()
    }

    /// Only the owner checkout serves the distributed control plane.
    fn ensure_distributed_owner_workspace(&self) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()
    }

    fn distributed_owner_machine_id(&self) -> Option<String> {
        self.workspace_owner_machine_id()
            .map(ToOwned::to_owned)
            .or_else(|| self.automation_machine_identity().map(ToOwned::to_owned))
    }

    /// Ship configuration as the owner would resolve it at admission.
    fn owner_ship_contract(&self) -> AdmissionShipContract {
        let base_branch = self.workspace_base_branch().to_string();
        AdmissionShipContract {
            mode: match self
                .workspace_runtime_binding()
                .map(|binding| binding.ship_mode)
            {
                Some(orbit_types::workflow::ShipMode::Local) => "local".to_string(),
                _ => "pr".to_string(),
            },
            landing_branch: base_branch.clone(),
            base_branch,
            review_policy: review_policy_label(self.operation_policy().review_policy.value),
            completion: "review".to_string(),
            authorization_reference: None,
        }
    }

    /// Evaluate the declared caller contract against the shared admission
    /// ladder, without creating anything.
    fn declared_contract_refusal(
        &self,
        session: &ToolSessionContext,
        declared: &DeclaredCallerContract,
        ship: &AdmissionShipContract,
        diagnostics: &mut Vec<String>,
    ) -> Result<Option<AdmissionRefusal>, OrbitError> {
        if ship.review_policy != "none" {
            diagnostics.push(format!(
                "owner review policy is '{}'; v1 admits only 'none'",
                ship.review_policy
            ));
        }
        if declared.is_empty() {
            // Nothing was declared, so there is nothing to compare. The owner's
            // own policy is still reported above and rechecked by pull.
            return Ok(
                (ship.review_policy != "none").then_some(AdmissionRefusal::ReviewPolicyUnsupported)
            );
        }
        let machine_id = session_machine_id(session).unwrap_or_else(|| "probe".to_string());
        let request = AdmissionRequest {
            // Probe placeholders: a probe declares no durable request identity
            // and no drain run, so these stand in for the shape checks a real
            // pull makes against its own values.
            request_id: "probe".to_string(),
            caller_version: declared.caller_version.clone().unwrap_or_default(),
            caller_schema: declared.caller_schema.unwrap_or_default(),
            caller_review_policy: declared
                .caller_review_policy
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            run_context: AdmissionRunContext {
                run_id: "probe".to_string(),
                job_name: "probe".to_string(),
                host_id: session.caller_host_id.clone(),
            },
            ship: ship.clone(),
        };
        let identity = trusted_identity(&machine_id, session);
        let refusal = orbit_store::admission_refusal(&identity, &request, owner_binary_version());
        if let Some(refusal) = refusal {
            diagnostics.push(match refusal {
                AdmissionRefusal::InvalidInput => {
                    "declared caller version, schema, or drain context is missing or malformed"
                        .to_string()
                }
                AdmissionRefusal::VersionMismatch => format!(
                    "caller declares binary {} / protocol schema {}; this owner serves {} / {}",
                    request.caller_version,
                    request.caller_schema,
                    owner_binary_version(),
                    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
                ),
                AdmissionRefusal::ShipModeUnsupported => format!(
                    "ship mode '{}' is not available to this caller",
                    request.ship.mode
                ),
                AdmissionRefusal::ReviewPolicyUnsupported => format!(
                    "review policy must be 'none' on both endpoints; owner '{}', executor '{}'",
                    request.ship.review_policy, request.caller_review_policy
                ),
            });
        }
        Ok(refusal)
    }
}

/// The binary version both endpoints compare. One definition so the probe
/// cannot report a version admission would not accept.
pub fn owner_binary_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn review_policy_label(policy: orbit_config::ReviewPolicy) -> String {
    match policy {
        orbit_config::ReviewPolicy::None => "none",
        orbit_config::ReviewPolicy::BeforePr => "before-pr",
        orbit_config::ReviewPolicy::AfterLanding => "after-landing",
    }
    .to_string()
}

/// Every distributed caller needs the workspace's `agent` capability; an
/// operator session holds it too.
fn ensure_session_agent_capability(session: &ToolSessionContext) -> Result<(), OrbitError> {
    if session.has_capability(McpCapability::Agent)
        || session.has_capability(McpCapability::Operator)
    {
        return Ok(());
    }
    Err(OrbitError::CapabilityRefused(
        "the distributed drain surface requires the workspace's agent capability".into(),
    ))
}

/// The machine a session may speak for. A remote session's forwarded label
/// names its receipt namespace and nothing else; a local session uses the
/// accepting machine's own identity.
fn session_machine_id(session: &ToolSessionContext) -> Option<String> {
    if is_remote(session) {
        return session
            .remote_caller_machine_id()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
    }
    session
        .process_machine_id
        .clone()
        .or_else(|| session.caller_machine_id.clone())
}

fn is_remote(session: &ToolSessionContext) -> bool {
    session
        .transport
        .is_some_and(|transport| transport != McpTransport::Local)
}

/// Build store-side identity from trusted session facts alone.
fn trusted_identity(machine_id: &str, session: &ToolSessionContext) -> AdmissionIdentity {
    let location = ExecutionLocation {
        machine_id: machine_id.to_string(),
        host_id: session.caller_host_id.clone(),
    };
    if is_remote(session) {
        AdmissionIdentity::trusted_remote(location)
    } else {
        AdmissionIdentity::trusted_local(location)
    }
}

fn probe_session(session: &ToolSessionContext) -> DrainProbeSession {
    DrainProbeSession {
        capabilities: session
            .effective_capabilities
            .iter()
            .map(ToString::to_string)
            .collect(),
        caller_machine_id: session_machine_id(session),
        remote: is_remote(session),
    }
}

fn lookup_guidance(outcome: &str) -> &'static str {
    match outcome {
        "found" => {
            "Historical evidence only. Launch authority comes from the current claim phase, and a \
             saved ship contract incompatible with this executor is preserved for explicit \
             recovery rather than rewritten."
        }
        "expired" => {
            "The receipt was compacted to a non-reusable tombstone. Replay returns \
             `request_expired`; it never yields a new task."
        }
        _ => {
            "`not_found` is not proof that an earlier request cannot still arrive. Retry the \
             original request ID while it remains admissible, or quiesce old sends and reconcile \
             on the owner; do not mint a replacement ID."
        }
    }
}

fn json_error(error: serde_json::Error) -> OrbitError {
    OrbitError::Store(error.to_string())
}
