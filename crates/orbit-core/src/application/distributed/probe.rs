//! The read-only admission probe, receipt reconciliation and claim listing.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{
    ADMISSION_RECEIPT_LOOKUP_SCHEMA, AdmissionLookup, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde::Serialize;

use super::contract::{
    DeclaredCallerContract, is_remote, owner_binary_version, session_machine_id, trusted_identity,
};
use crate::application::review::{ReviewSwitches, review_switches};
use crate::runtime::authorization::resolved_caller_capabilities;

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
    /// Fingerprint derived from this build's pull request and nested types.
    pub protocol_fingerprint: String,
    pub binary_version: String,
    pub workspace_id: String,
    /// Registered owner machine, absent on a standalone registry that predates
    /// machine identity.
    pub owner_machine_id: Option<String>,
    /// Session facts, for diagnostics. Capabilities are what the destination
    /// resolved for this caller — the set the chokepoint admitted the call on,
    /// which on the CLI comes from the process envelope rather than from the
    /// session; the caller machine is attribution only.
    pub session: DrainProbeSession,
    /// Ship configuration the owner would resolve at admission.
    pub ship: AdmissionShipContract,
    /// The owner's review switches. `review.before_pr` and
    /// `review.before_landing` take part in admission — with either on, each
    /// claimed PR leaf runs the review the ship contract captures
    /// [ORB-13908] [ORB-14849]; after-landing
    /// review (the `delivery-code-review` auto-task) is reported for context
    /// and never refuses a pull [ORB-13992].
    pub review: ReviewSwitches,
    /// `true` when the caller would pass the admission ladder now.
    /// Undeclared optional caller fields are unknown and skip only the legs
    /// that compare those fields; owner-resolved ship mode and the owner's
    /// review switches are always evaluated.
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
    /// Refusal order matches the spec ladder: caller capability first, at the
    /// tool chokepoint every surface traverses, then the selector resolved by
    /// the surface that opened this runtime, then destination authority (a
    /// replica serves no control-plane work), then the store-owned
    /// shape/version/mode/policy ladder — which the probe reports rather than
    /// raising, because observing a mismatch before admission is the whole
    /// point of a preflight.
    pub fn drain_probe(
        &self,
        session: &ToolSessionContext,
        declared: &DeclaredCallerContract,
    ) -> Result<DrainProbeReport, OrbitError> {
        self.ensure_distributed_owner_workspace()?;
        let fingerprint = orbit_store::contracts::distributed_drain_protocol_fingerprint();
        if let Some(caller) = declared.caller_fingerprint.as_deref()
            && caller != fingerprint
        {
            return Err(OrbitError::ProtocolSkew(format!(
                "caller fingerprint {caller}; owner fingerprint {fingerprint}; deploy matching builds on both endpoints and restart their long-lived processes"
            )));
        }
        let ship = self.owner_ship_contract();
        let mut diagnostics = Vec::new();
        let refusal = self.declared_contract_refusal(session, declared, &ship, &mut diagnostics)?;
        Ok(DrainProbeReport {
            schema_version: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            protocol_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            protocol_fingerprint: fingerprint.to_string(),
            binary_version: owner_binary_version().to_string(),
            workspace_id: self.workspace_id()?,
            owner_machine_id: self.distributed_owner_machine_id(),
            session: probe_session(session),
            review: review_switches(self, Utc::now())?,
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
            return Err(OrbitError::InvalidInput("`request_id` is required".into()));
        }
        let session_machine = session_machine_id(session);
        let machine_id = match requested_machine.map(str::trim).filter(|m| !m.is_empty()) {
            None => session_machine.clone().ok_or_else(|| {
                OrbitError::InvalidInput(
                    "no trusted caller machine on this session; name the original \
                     caller machine with `machine_id` from an operator session"
                        .into(),
                )
            })?,
            Some(requested) => {
                // Input-dependent, so it cannot be a tool-name-keyed governed
                // row; it asks the shared resolution instead of the session so
                // the answer matches the chokepoint's on every surface.
                if !resolved_caller_capabilities(session).contains(&McpCapability::Operator)
                    && session_machine.as_deref() != Some(requested)
                {
                    return Err(OrbitError::CapabilityRefused(
                        "cross-attempt receipt inspection requires operator capability; a worker \
                         session reads its own receipt namespace. Omit `machine_id` to read it, \
                         or re-run as an operator (`ORBIT_OPERATOR=1` from a shell, or a session \
                         served by `orbit mcp serve --operator`)"
                            .into(),
                    ));
                }
                requested.to_string()
            }
        };
        let identity = trusted_identity(&machine_id, session, None);
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
}

fn probe_session(session: &ToolSessionContext) -> DrainProbeSession {
    DrainProbeSession {
        capabilities: resolved_caller_capabilities(session)
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
