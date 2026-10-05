//! The owner's side of the caller contract: the mutation gate, the ship
//! contract it resolves, the session identity it trusts, and the declared
//! contract it checks against the shared admission ladder.

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionRefusal, AdmissionRequest, AdmissionRunContext,
    AdmissionShipContract, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, ExecutionLocation,
};
use orbit_types::tool::{McpTransport, ToolSessionContext};

/// Whether the mutating distributed entry points are reachable from any public
/// surface.
///
/// It is a constant rather than configuration on purpose: the feature must not
/// become reachable, or unreachable, because an operator set a key or an
/// environment variable. [ORB-13625] opened it once the routed follower peer,
/// the owner's pull/bind/settle tools and `orbit run auto --pull` landed
/// together; every gated entry point still names it, so closing the feature
/// again is one deliberate source change.
pub const DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED: bool = true;

/// The authorization reference an owner-policy completion carries: the config
/// key that grants it. A claim admitted under `completion: done` pins this
/// value, and a handoff is authorized from it only while the owner's current
/// configuration still names it.
pub const OWNER_COMPLETION_POLICY: &str = "workspace-config:workflow.distributed_completion";

/// Refuse a mutating distributed entry point while the feature is closed.
pub fn ensure_distributed_mutation_available(entry_point: &str) -> Result<(), OrbitError> {
    if DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED {
        return Ok(());
    }
    Err(OrbitError::CapabilityRefused(format!(
        "distributed entry point '{entry_point}' is unavailable: the distributed drain's \
         mutating entry points are closed in this build, so the owner serves only the read-only \
         preflight and receipt lookup"
    )))
}

/// What the caller declares about itself, so the probe can answer the question
/// a follower actually has: *would my pull be admitted right now?*
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredCallerContract {
    pub caller_version: Option<String>,
    pub caller_schema: Option<u32>,
    pub caller_before_pr: Option<bool>,
}

impl crate::OrbitRuntime {
    /// This host's `review.before_pr`. A follower declares it on every probe;
    /// a pull declares the value its drain captured at submission
    /// [ORB-13992]. After-landing review never enters the protocol.
    pub(crate) fn local_review_before_pr(&self) -> bool {
        self.operation_policy().review_before_pr.value
    }

    /// Only the owner checkout serves the distributed control plane.
    ///
    /// A replica is refused with what to do instead: these tools answer for
    /// the owner, so the caller has to reach the owner rather than this
    /// checkout. Every owner-side drain tool, read-only ones included, comes
    /// through here, which is why the message does not talk about writes.
    pub(super) fn ensure_distributed_owner_workspace(&self) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()
            .map_err(|error| match (&error, self.coordination_write_owner()) {
                (OrbitError::CapabilityRefused(_), Some(owner)) => {
                    OrbitError::CapabilityRefused(format!(
                        "this checkout is a replica of machine '{owner}'; the distributed \
                         drain's owner tools (probe, receipt lookup, claims, pull) are served by \
                         the owner, not a replica checkout. Run them on the owner's checkout, or \
                         call them through the owner's federated selector"
                    ))
                }
                _ => error,
            })
    }

    pub(super) fn distributed_owner_machine_id(&self) -> Option<String> {
        self.workspace_owner_machine_id()
            .map(ToOwned::to_owned)
            .or_else(|| self.automation_machine_identity().map(ToOwned::to_owned))
    }

    /// Ship configuration as the owner would resolve it at admission.
    pub(super) fn owner_ship_contract(&self) -> AdmissionShipContract {
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
            before_pr: self.local_review_before_pr(),
            completion: self.workflow_distributed_completion().to_string(),
            authorization_reference: self.owner_completion_authority(),
        }
    }

    /// The standing completion authority this owner grants claimed handoffs
    /// right now, or `None` when each one waits for an operator's approval.
    ///
    /// It names the owner's own configuration, never anything a follower
    /// sent. Admission pins it into the claim's ship contract, acceptance
    /// records it as the handoff's authorization, and landing rechecks it, so
    /// withdrawing the key stops every handoff that has not landed yet.
    pub(crate) fn owner_completion_authority(&self) -> Option<String> {
        (self.workflow_distributed_completion() == "done")
            .then(|| OWNER_COMPLETION_POLICY.to_string())
    }

    /// Evaluate the declared caller contract against the shared admission
    /// ladder, without creating anything.
    pub(super) fn declared_contract_refusal(
        &self,
        session: &ToolSessionContext,
        declared: &DeclaredCallerContract,
        ship: &AdmissionShipContract,
        diagnostics: &mut Vec<String>,
    ) -> Result<Option<AdmissionRefusal>, OrbitError> {
        if ship.before_pr {
            diagnostics.push(
                "owner has review.before_pr on; pulled leaves require it off on both endpoints"
                    .to_string(),
            );
        }
        let machine_id = session_machine_id(session).unwrap_or_else(|| "probe".to_string());
        let request = AdmissionRequest {
            // Probe placeholders: a probe declares no durable request identity
            // and no drain run, so these stand in for the shape checks a real
            // pull makes against its own values. Undeclared optional caller
            // fields are unknown, not empty: fill the owner-matching value so
            // those caller-dependent legs are skipped while owner-resolved
            // ship mode and the owner's review.before_pr still run.
            request_id: "probe".to_string(),
            caller_version: declared
                .caller_version
                .clone()
                .unwrap_or_else(|| owner_binary_version().to_string()),
            caller_schema: declared
                .caller_schema
                .unwrap_or(DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA),
            caller_before_pr: declared.caller_before_pr.unwrap_or(false),
            run_context: AdmissionRunContext {
                run_id: "probe".to_string(),
                job_name: "probe".to_string(),
                machine_name: session.caller_machine_name.clone(),
            },
            ship: ship.clone(),
            crews: None,
            os: None,
        };
        let identity = trusted_identity(&machine_id, session);
        let refusal = orbit_store::admission_refusal(&identity, &request, owner_binary_version());
        if let Some(refusal) = refusal {
            diagnostics.push(match refusal {
                AdmissionRefusal::InvalidInput => {
                    "declared caller version, schema, or drain context is missing or malformed"
                        .to_string()
                }
                AdmissionRefusal::ProtocolMismatch => format!(
                    "protocol_mismatch: caller revision {}; owner revision {}",
                    request.caller_schema, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
                ),
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
                AdmissionRefusal::BeforePrUnsupported => format!(
                    "review.before_pr must be off on both endpoints; owner {}, executor {}",
                    on_off(request.ship.before_pr),
                    on_off(request.caller_before_pr)
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

pub(super) fn on_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// The machine a session may speak for. A remote session's forwarded label
/// names its receipt namespace and nothing else; a local session uses the
/// accepting machine's own identity.
pub(super) fn session_machine_id(session: &ToolSessionContext) -> Option<String> {
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

pub(super) fn is_remote(session: &ToolSessionContext) -> bool {
    session
        .transport
        .is_some_and(|transport| transport != McpTransport::Local)
}

/// Build store-side identity from trusted session facts alone.
pub(super) fn trusted_identity(
    machine_id: &str,
    session: &ToolSessionContext,
) -> AdmissionIdentity {
    let location = ExecutionLocation {
        machine_id: machine_id.to_string(),
        machine_name: session.caller_machine_name.clone(),
    };
    if is_remote(session) {
        AdmissionIdentity::trusted_remote(location)
    } else {
        AdmissionIdentity::trusted_local(location)
    }
}

/// Compare the probe's contract revision before sending any admission fields.
/// Older owners call this `version_mismatch`; the follower still reports the
/// protocol-specific refusal with both revisions.
pub(crate) fn protocol_mismatch(report: &serde_json::Value) -> Option<String> {
    let owner = report
        .get("protocol_schema")
        .and_then(serde_json::Value::as_u64);
    (owner != Some(u64::from(DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA))).then(|| {
        let owner = owner.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        format!(
            "protocol_mismatch: caller revision {}; owner revision {owner}; deploy matching \
             protocol revisions on both endpoints and restart their long-lived processes",
            DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
        )
    })
}
