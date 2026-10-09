//! Admission authority, wire protocol, and receipt contracts.

use serde::{Deserialize, Serialize};

use super::{ClaimCandidateRef, ExecutionClaim};

pub use orbit_types::task::ExecutionLocation;

/// Owner-side ordering facts, separate from the executor's wire request.
#[derive(Debug, Default)]
pub struct AdmissionOrdering {
    /// The owner's OS. Outside the namespace, it runs only unrestricted tasks.
    pub owner_os: Option<orbit_types::task::HostOs>,
    /// Tasks whose admitted frozen delivery batch nears its deadline.
    pub expiring_tasks: std::collections::BTreeSet<String>,
}

/// Authority supplied by the embedding runtime, never deserialized from tool input.
/// The caller must already have session agent capability. SSH login establishes owner
/// access; remote machine labels are attribution, not destination credentials.
#[derive(Debug, Clone)]
pub struct AdmissionIdentity {
    location: ExecutionLocation,
    remote: bool,
}

impl AdmissionIdentity {
    pub fn trusted_local(location: ExecutionLocation) -> Self {
        Self {
            location,
            remote: false,
        }
    }

    pub fn trusted_remote(location: ExecutionLocation) -> Self {
        Self {
            location,
            remote: true,
        }
    }

    pub fn location(&self) -> &ExecutionLocation {
        &self.location
    }
    pub fn is_remote(&self) -> bool {
        self.remote
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionRunContext {
    pub run_id: String,
    pub job_name: String,
    pub machine_name: Option<String>,
}

/// Owner-resolved configuration, included in immutable retry comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionShipContract {
    pub mode: String,
    pub base_branch: String,
    pub landing_branch: String,
    /// The owner's `review.before_pr` when it resolved this contract
    /// [ORB-13992]. Contracts recorded before revision 5 carried a
    /// `review_policy` admitted only as `none`, which reads as `false`.
    #[serde(default)]
    pub before_pr: bool,
    /// The owner's `review.before_landing` when it resolved this contract
    /// [ORB-14849]: the claimed leaf reviews its open pull request before
    /// handing it off. Never on together with `before_pr`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub before_landing: bool,
    pub completion: String,
    pub authorization_reference: Option<String>,
    /// The owner's captured review contract: present exactly when
    /// `before_pr` [ORB-13895] or `before_landing` [ORB-14849] is on. A
    /// handoff's review evidence is judged against it, never against the
    /// owner's settings at handoff time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<AdmissionReviewContract>,
}

/// The review a claimed leaf must run before it hands off, as the owner
/// resolved it when the claim was admitted [ORB-13895]. Its timing is the
/// ship contract's `before_pr` or `before_landing`; the rest is what the
/// leaf's gate and the owner's acceptance hold it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionReviewContract {
    /// The review evidence contract version the owner reads
    /// (`REVIEW_CONTRACT_VERSION`). A certificate under another version is
    /// refused rather than reinterpreted.
    pub contract_version: u32,
    /// The owner's `operation.review_crew`, when one is set. A handoff's
    /// reviewer must be this crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The owner's `review.minutes` budget for the leaf's review.
    pub budget: orbit_types::workflow::ReviewBudget,
    /// Owner-required candidate checks captured at claim admission. `None`
    /// means this is a legacy ship contract without enough evidence to
    /// establish review validation; an empty list is an explicit no-check
    /// contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_validation_commands: Option<Vec<String>>,
    /// The owner's `review.baseline_commands` captured at claim admission
    /// [ORB-14684]. The leaf's settlement reruns only these and the required
    /// commands on the base, and refuses a failure of either filed as a
    /// `diagnostic`; the certificate must carry the same list. Absent on
    /// contracts captured before the snapshot: no trusted baseline commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline_commands: Vec<String>,
    /// The owner's `review.host_evidence` rules: checks the leaf's host owes
    /// for the paths its candidate changed, which the leaf's review gate
    /// synthesizes as requirements whatever its reviewer reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_evidence: Vec<orbit_types::workflow::HostEvidenceRule>,
}

impl AdmissionShipContract {
    /// The review timing this contract captured for the claimed leaf.
    #[must_use]
    pub fn review_timing(&self) -> orbit_types::workflow::ReviewTiming {
        use orbit_types::workflow::ReviewTiming;
        if self.before_pr {
            ReviewTiming::BeforePr
        } else if self.before_landing {
            ReviewTiming::BeforeLanding
        } else {
            ReviewTiming::None
        }
    }

    /// Whether `review` matches the captured timing and is itself well
    /// formed.
    #[must_use]
    pub fn review_contract_consistent(&self) -> bool {
        match &self.review {
            None => !self.before_pr && !self.before_landing,
            Some(review) => {
                self.before_pr != self.before_landing
                    && review.contract_version == orbit_types::workflow::REVIEW_CONTRACT_VERSION
                    && review
                        .crew
                        .as_deref()
                        .is_none_or(|crew| !crew.trim().is_empty())
                    && review.required_validation_commands.is_some()
            }
        }
    }
}

/// Wire-protocol version of pull, probe, and lifecycle request/response shapes.
///
/// It versions the distributed-drain protocol alone, not the scoreboard's
/// `ORCHESTRATION_SCHEMA_VERSION` and not MCP's own initialize metadata.
/// Retained for persisted requests and lifecycle semantics. Pull request field
/// compatibility is now derived by [`distributed_drain_protocol_fingerprint`],
/// so an additive field needs no manual bump to detect skew. Revision 2 adds executor crew capabilities;
/// revision 3 adds handoff footprint widening; revision 4 adds the executor's
/// host OS; revision 5 replaces both endpoints' review-policy labels with
/// their captured `review.before_pr` switch [ORB-13992]; revision 6 adds the
/// ship contract's captured `review`, the executor's `review_gate` and the
/// handoff's before-PR review evidence [ORB-13895]; revision 7 sends
/// `review_gate` on `orbit.task.pull`, which revision 6 owners reject
/// [ORB-13908]; revision 8 captures the owner's required validation
/// commands in the before-PR review contract [ORB-14192]; revision 9 adds
/// NoDiff claim delivery and owner-verified clean-base completion [ORB-14259];
/// revision 10 adds the settlement's typed failure, the `leaf_released` crew
/// exclusion source and the receipt's resumable candidate [ORB-14257].
pub const DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA: u32 = 10;

/// The first protocol revision whose claimed leaf hands off `NoDiff`
/// [ORB-14259]. Admission offers `no-diff-expected` work only to an executor
/// at this revision or later.
pub const NO_DIFF_HANDOFF_PROTOCOL_SCHEMA: u32 = 9;

/// The pull wire shape, derived from the same request and nested types that
/// admission deserializes. No field list or manually bumped revision can drift
/// from these types. Keep the draft explicit across generator upgrades.
pub fn admission_request_schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        schemars::generate::SchemaSettings::draft07()
            // Prose edits must not create wire skew. Transform schema nodes,
            // preserving request properties actually named title/description.
            .with_transform(schemars::transform::RecursiveTransform(
                |schema: &mut schemars::Schema| {
                    schema.remove("title");
                    schema.remove("description");
                },
            ))
            .into_generator()
            .into_root_schema_for::<AdmissionRequest>()
            .to_value()
    })
}

/// SHA-256 of the generated pull request schema, including nested contracts.
/// Both endpoints compute this from their running build, independently of the
/// release version and the legacy protocol revision.
pub fn distributed_drain_protocol_fingerprint() -> &'static str {
    static FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        orbit_common::security::release::sha256_hex(
            admission_request_schema().to_string().as_bytes(),
        )
    })
}

/// Receipt-lookup schema, versioned independently of admission so a client
/// upgraded to the owner's binary can reconcile an old request without
/// rewriting the input that request was made with.
pub const ADMISSION_RECEIPT_LOOKUP_SCHEMA: u32 = 1;

/// Ordered pre-admission refusal classes, in the order a caller sees them.
///
/// Selector resolution and session capability are decided by the calling
/// surface before this ladder, because only that surface knows which workspace
/// was addressed and which capabilities the session holds. Everything below is
/// store-owned and shared by admission and the read-only preflight, so a probe
/// cannot report a verdict admission would not reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionRefusal {
    InvalidInput,
    ProtocolSkew,
    ProtocolMismatch,
    VersionMismatch,
    ShipModeUnsupported,
    /// The owner captured `review.before_pr` or `review.before_landing` on
    /// and the executor's leaf does not declare that it runs the review
    /// gate, or the ship mode is not the PR route the gate runs on
    /// [ORB-13908] [ORB-14849]. The executor's own switch never refuses.
    /// After-landing review never refuses: it runs on the owner after
    /// landing.
    #[serde(alias = "review_policy_unsupported")]
    BeforePrUnsupported,
}

impl AdmissionRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::ProtocolSkew => "protocol_skew",
            Self::VersionMismatch => "version_mismatch",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::ShipModeUnsupported => "ship_mode_unsupported",
            Self::BeforePrUnsupported => "before_pr_unsupported",
        }
    }
}

/// The crews a pulling executor can run, declared on each request [ORB-13941].
///
/// The owner admits a candidate only when the crew it would run as on this
/// executor — its own `task.crew`, or `default_crew` for a task naming none —
/// is runnable here. A task the executor cannot run is skipped, not refused:
/// it stays in the backlog for the owner or another follower.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionCrewCapability {
    /// Crews the executor's window preflight found runnable, by registry
    /// name. `None` when the window took no preflight: then any crew not in
    /// `excluded` is admissible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runnable: Option<Vec<String>>,
    /// The crew a task naming none runs as on the executor. `None` admits no
    /// crew-less task, since its leaf would have no crew to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_crew: Option<String>,
    /// Crews never admitted to this executor for the rest of its window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<orbit_types::workflow::CrewExclusion>,
}

impl AdmissionCrewCapability {
    /// Why a task whose own crew is `task_crew` cannot run on this executor,
    /// or `None` when it can.
    #[must_use]
    pub fn unrunnable_reason(&self, task_crew: Option<&str>) -> Option<String> {
        let named = task_crew.map(str::trim).filter(|crew| !crew.is_empty());
        let Some(crew) = named.or(self.default_crew.as_deref()) else {
            return Some(
                "names no crew and the executor has no default crew to run it as".to_string(),
            );
        };
        let via = if named.is_some() {
            String::new()
        } else {
            " (the executor's default crew)".to_string()
        };
        if let Some(exclusion) = self
            .excluded
            .iter()
            .find(|exclusion| exclusion.crew == crew)
        {
            return Some(format!(
                "crew `{crew}`{via} is excluded on the executor: {}",
                exclusion.reason
            ));
        }
        match &self.runnable {
            Some(runnable) if !runnable.iter().any(|name| name == crew) => Some(format!(
                "crew `{crew}`{via} is not runnable on the executor"
            )),
            _ => None,
        }
    }

    fn malformed(&self) -> bool {
        let blank = |name: &str| name.trim().is_empty();
        self.runnable.iter().flatten().any(|name| blank(name))
            || self.default_crew.as_deref().is_some_and(blank)
            || self.excluded.iter().any(|exclusion| blank(&exclusion.crew))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionRequest {
    pub request_id: String,
    pub caller_version: String,
    pub caller_schema: u32,
    /// Type-derived request fingerprint. Optional when reading persisted
    /// requests from before fingerprint negotiation; new followers declare it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_fingerprint: Option<String>,
    /// The executor's captured `review.before_pr` [ORB-13992]. Diagnostic
    /// only: a claimed leaf runs the review the ship contract captured, never
    /// the executor's own [ORB-13908]. Requests recorded before revision 5
    /// carried a `caller_review_policy`, which reads as `false`.
    #[serde(default)]
    pub caller_before_pr: bool,
    /// Whether the executor's claimed leaf runs the before-PR gate the ship
    /// contract's `review` captures [ORB-13895]. An owner with
    /// `review.before_pr` on admits only an executor that declares it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review_gate: bool,
    pub run_context: AdmissionRunContext,
    pub ship: AdmissionShipContract,
    /// What the executor can run. Absent for an owner-local admission and
    /// for callers that place no crew restriction: then every crew is
    /// admissible, as before [ORB-13941].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crews: Option<AdmissionCrewCapability>,
    /// The executor's host OS, matched against each candidate's `os:` tags.
    /// Absent from an executor on an OS outside the `os:` namespace: only
    /// tasks without an `os:` tag are admitted to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<orbit_types::task::HostOs>,
}

impl AdmissionRequest {
    /// Whether the declared crew capability is malformed (a blank name).
    #[must_use]
    pub fn crews_malformed(&self) -> bool {
        self.crews
            .as_ref()
            .is_some_and(AdmissionCrewCapability::malformed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionDiagnostic {
    pub task_id: String,
    pub reason: String,
    /// The tasks it waits on or is held behind, when the owner knows them: the
    /// unfinished dependencies, or the holder of the protected footprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionTaskSummary {
    pub id: String,
    pub title: String,
    pub complexity: Option<orbit_types::task::TaskComplexity>,
    pub crew: Option<String>,
    pub context_files: Vec<String>,
    /// [ORB-14257] The candidate an earlier claim of the task preserved,
    /// for this claim's leaf to resume instead of implementing afresh. The
    /// owner offers it only while no operator discarded it and the task's
    /// spec is unchanged since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_candidate: Option<ClaimCandidateRef>,
}

/// Original response, immutable even when the current claim moves on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionReceipt {
    pub schema_version: u32,
    pub request: AdmissionRequest,
    pub machine_id: String,
    pub claim: Option<ExecutionClaim>,
    pub task: Option<AdmissionTaskSummary>,
    pub invalid_candidates: Vec<AdmissionDiagnostic>,
    pub deferred_conflicts: Vec<AdmissionDiagnostic>,
    /// Ready candidates skipped because the executor cannot run their crew.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crew_unavailable: Vec<AdmissionDiagnostic>,
    /// Ready candidates skipped because their `os:` tags name no OS the
    /// executor runs. They stay in the backlog for a host that does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub os_unavailable: Vec<AdmissionDiagnostic>,
    pub queue_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionLookup {
    Found {
        receipt: Box<AdmissionReceipt>,
        current_claim: Option<Box<ExecutionClaim>>,
    },
    Expired,
    NotFound,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionStorageUsage {
    pub receipts: u64,
    pub tombstones: u64,
    /// Logical UTF-8 payload bytes; excludes SQLite page/index overhead.
    pub receipt_bytes: u64,
    pub tombstone_bytes: u64,
}
