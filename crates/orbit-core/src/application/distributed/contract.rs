//! The owner's side of the caller contract: the mutation gate, the ship
//! contract it resolves, the session identity it trusts, and the declared
//! contract it checks against the shared admission ladder.

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionRefusal, AdmissionRequest, AdmissionReviewContract,
    AdmissionRunContext, AdmissionShipContract, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
    ExecutionLocation,
};
use orbit_types::tool::{McpTransport, ToolSessionContext};
use orbit_types::workflow::REVIEW_CONTRACT_VERSION;

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
    /// Type-derived pull request fingerprint, declared after discovering that
    /// the owner supports fingerprint negotiation.
    pub caller_fingerprint: Option<String>,
    pub caller_before_pr: Option<bool>,
}

impl crate::OrbitRuntime {
    /// This host's `review.before_pr`. A follower declares it on every probe;
    /// a pull declares the value its drain captured at submission
    /// [ORB-13992]. After-landing review never enters the protocol.
    pub(crate) fn local_review_before_pr(&self) -> bool {
        self.operation_policy().review_before_pr.value
    }

    /// Why this executor cannot run the before-PR review the owner's ship
    /// contract captured, or `None` when it can or none is captured
    /// [ORB-13908]. A claimed leaf reviews with exactly the captured crew, so
    /// a follower that cannot resolve it refuses before it claims anything
    /// rather than claiming a task its gate would escalate.
    pub(crate) fn claimed_review_refusal(&self, ship: &AdmissionShipContract) -> Option<String> {
        let review = ship.review.as_ref()?;
        let Some(crew) = review.crew.as_deref() else {
            return Some(
                "before_pr_reviewer_unavailable: the owner has review.before_pr on but no \
                 operation.review_crew; a claimed leaf never reviews with its implementer's crew"
                    .to_string(),
            );
        };
        self.resolve_crew_for_task(Some(crew), None)
            .err()
            .map(|error| {
                format!(
                    "before_pr_reviewer_unavailable: the owner's before-PR review crew `{crew}` \
                     cannot be resolved on this executor: {error}"
                )
            })
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
    ///
    /// With `review.before_pr` on it also captures the review contract a
    /// claimed leaf's gate and the owner's acceptance are held to
    /// [ORB-13895]. It carries no capture time, so a follower that echoes
    /// the probed contract still matches the owner's current resolution.
    pub(super) fn owner_ship_contract(&self) -> AdmissionShipContract {
        let base_branch = self.workspace_base_branch().to_string();
        let policy = self.operation_policy();
        let before_pr = self.local_review_before_pr();
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
            before_pr,
            completion: self.workflow_distributed_completion().to_string(),
            authorization_reference: self.owner_completion_authority(),
            review: before_pr.then(|| AdmissionReviewContract {
                contract_version: REVIEW_CONTRACT_VERSION,
                crew: policy.review_crew.value.clone(),
                budget: policy.review_budget(),
                required_validation_commands: Some(
                    self.workflow_required_validation_commands().to_vec(),
                ),
                baseline_commands: self.review_baseline_commands().to_vec(),
                host_evidence: policy.review_host_evidence.value.clone(),
            }),
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
        if let Some(review) = &ship.review {
            diagnostics.push(format!(
                "owner has review.before_pr on; each claimed leaf runs the before-PR review with                  crew {} before it opens a pull request",
                review
                    .crew
                    .as_deref()
                    .map_or_else(|| "(unset)".to_string(), |crew| format!("`{crew}`"))
            ));
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
            caller_fingerprint: declared.caller_fingerprint.clone(),
            // Every executor of this binary runs the before-PR gate on its
            // claimed PR leaves [ORB-13908].
            review_gate: true,
            run_context: AdmissionRunContext {
                run_id: "probe".to_string(),
                job_name: "probe".to_string(),
                machine_name: session.caller_machine_name.clone(),
            },
            ship: ship.clone(),
            crews: None,
            os: None,
        };
        let identity = trusted_identity(&machine_id, session, None);
        let refusal = orbit_store::admission_refusal(&identity, &request, owner_binary_version());
        if let Some(refusal) = refusal {
            diagnostics.push(match refusal {
                AdmissionRefusal::InvalidInput => {
                    "declared caller version, schema, or drain context is missing or malformed"
                        .to_string()
                }
                AdmissionRefusal::ProtocolSkew => "pull request schema fingerprints differ".to_string(),
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
                    "owner has review.before_pr on; the before-PR review runs only on the PR                      route, and this owner ships '{}'",
                    request.ship.mode
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

/// Build store-side identity from session facts. The request may supply a
/// display name when the transport has none; it never supplies the machine id.
pub(super) fn trusted_identity(
    machine_id: &str,
    session: &ToolSessionContext,
    display_name: Option<&str>,
) -> AdmissionIdentity {
    let location = ExecutionLocation {
        machine_id: machine_id.to_string(),
        machine_name: session
            .caller_machine_name
            .clone()
            .or_else(|| display_name.map(ToOwned::to_owned)),
    };
    if is_remote(session) {
        AdmissionIdentity::trusted_remote(location)
    } else {
        AdmissionIdentity::trusted_local(location)
    }
}

/// Compare the owner's derived request shape before sending admission fields.
/// Missing fingerprints and legacy revision mismatches fail by the same type.
pub(crate) fn protocol_skew(report: &serde_json::Value) -> Option<OrbitError> {
    // Older owners may return a scrubbed identity instead of a typed error.
    // That says nothing about build compatibility and must not end a drain.
    if let Some(field) = crate::runtime::tool_exec::corrupted_drain_identity(report) {
        return Some(OrbitError::OwnerNegotiation(format!(
            "owner reply identity field `{field}` contains an environment redaction artefact; retry next pass"
        )));
    }
    let owner = report
        .get("protocol_schema")
        .and_then(serde_json::Value::as_u64);
    let fingerprint = report
        .get("protocol_fingerprint")
        .and_then(serde_json::Value::as_str);
    let caller = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    (owner != Some(u64::from(DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA)) || fingerprint != Some(caller)).then(|| {
        let owner = owner.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        OrbitError::ProtocolSkew(format!(
            "caller revision {}; owner revision {owner}; caller fingerprint {caller}; owner fingerprint {}; deploy matching builds on both endpoints and restart their long-lived processes",
            DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, fingerprint.unwrap_or("unavailable")
        ))
    })
}

/// Discover using fields older owners accept, then exchange fingerprints only
/// with an owner whose response matches. A legacy owner lacking a fingerprint
/// is refused locally by type, before it can reject a new probe or pull field.
pub(crate) fn probe_pull_contract(
    transport: &dyn orbit_tools::DrainOwnerTransport,
    selector: &str,
    before_pr: bool,
) -> Result<serde_json::Value, OrbitError> {
    let mut input = serde_json::json!({
        "caller_version": owner_binary_version(),
        "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        "caller_before_pr": before_pr,
    });
    let report = transport
        .call(selector, "orbit.drain.probe", input.clone())
        .map_err(owner_protocol_error)?;
    if let Some(error) = protocol_skew(&report) {
        return Err(error);
    }
    input["caller_fingerprint"] =
        serde_json::json!(orbit_store::contracts::distributed_drain_protocol_fingerprint());
    let report = transport
        .call(selector, "orbit.drain.probe", input)
        .map_err(owner_protocol_error)?;
    if let Some(error) = protocol_skew(&report) {
        return Err(error);
    }
    Ok(report)
}

/// Recover the typed protocol refusal across a structured remote tool error.
pub(crate) fn owner_protocol_error(error: OrbitError) -> OrbitError {
    match error {
        OrbitError::RemoteTool { code, message, .. } if code == "protocol_skew" => {
            OrbitError::ProtocolSkew(
                message
                    .strip_prefix("protocol_skew: ")
                    .unwrap_or(&message)
                    .to_string(),
            )
        }
        other => other,
    }
}
