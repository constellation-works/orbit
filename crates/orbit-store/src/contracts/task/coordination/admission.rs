//! Admission authority, wire protocol, and receipt contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ClaimCandidateRef, ExecutionClaim};

pub use orbit_types::task::ExecutionLocation;

/// Owner-side facts admission reads, separate from the executor's wire
/// request: how it orders candidates, and what its receipt captures.
#[derive(Debug, Default)]
pub struct AdmissionOrdering {
    /// How far work only the requester can run reaches in the order
    /// (`workflow.admission_affinity`) [ORB-17304].
    pub affinity: orbit_types::task::AdmissionAffinityReach,
    /// The live executors the requester's candidates are ordered against,
    /// the requester among them. `None`, when the census could not be read
    /// or `affinity` is off, orders canonically.
    pub census: Option<super::ExecutorCensus>,
    /// Tasks whose admitted frozen delivery batch nears its deadline.
    pub expiring_tasks: std::collections::BTreeSet<String>,
    /// The owner's validation gates, captured in the receipt
    /// ([`AdmissionReceipt::gates`]) [ORB-15622].
    pub gates: Option<orbit_types::workflow::ValidationGates>,
    /// The owner's tag instructions, captured in the receipt
    /// ([`AdmissionReceipt::tag_instructions`]).
    pub tag_instructions: Option<orbit_types::workflow::CapturedTagInstructions>,
    /// Crew name to the executor `host_os` that constrains tasks assigned to
    /// it. Empty leaves admission on the task's `os:` tags alone. Not part of
    /// the follower's request, so it does not change the request fingerprint.
    pub executor_host_os: BTreeMap<String, orbit_types::task::ExecutorOsLimit>,
    /// Offer only a pending landing repair, never a backlog task: the
    /// owner's own repair pull, which its `orbit run auto` drain starts once
    /// a repair's fallback window has passed.
    pub repairs_only: bool,
    /// The owner's crew pools a candidate may draw from, by pool key: a
    /// complexity tier (`hard`) or `purpose:<purpose>`, each naming its
    /// member crews. Read only to score a candidate by the requester's
    /// reported usage pressure [ORB-17773]; empty scores every candidate
    /// neutral. Owner-side facts, like `executor_host_os`, so they do not
    /// change the request fingerprint.
    pub crew_pools: BTreeMap<String, Vec<String>>,
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
    /// Whether the owner's `review.timing` was `before_pr` when it resolved this contract
    /// [ORB-13992]. Contracts recorded before revision 5 carried a
    /// `review_policy` admitted only as `none`, which reads as `false`.
    #[serde(default)]
    pub before_pr: bool,
    /// Whether the owner's `review.timing` was `before_landing` when it resolved this contract
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
    /// The owner's `workflow.purpose_crews.review`, when one is set. A handoff's
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
/// It versions the distributed-drain protocol alone, not the Cost page's
/// `ORCHESTRATION_SCHEMA_VERSION` and not MCP's own initialize metadata.
/// Retained for persisted requests and lifecycle semantics. Pull request field
/// compatibility is now derived by [`distributed_drain_protocol_fingerprint`],
/// so an additive field needs no manual bump to detect skew. Revision 2 adds executor crew capabilities;
/// revision 3 adds handoff footprint widening; revision 4 adds the executor's
/// host OS; revision 5 replaces both endpoints' review-policy labels with
/// their captured before-PR review switch [ORB-13992]; revision 6 adds the
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
    /// The owner captured a `review.timing` of `before_pr` or `before_landing`
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
/// One crew's weekly usage pressure on the pulling executor, in
/// milli-units (1000 = pressure 1.0) [ORB-17773]. An integer keeps the
/// capability `Eq` and the ordering key exact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionCrewPressure {
    /// Configured crew name.
    pub crew: String,
    /// Raw weekly pressure ×1000, rounded; higher expires sooner unused.
    pub pressure_milli: u64,
}

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
    /// Per-crew weekly usage pressure from the executor's own readings
    /// [ORB-17773]. Optional and additive: absent, or a crew it does not
    /// name, reads as neutral pressure 1.0. The owner only orders by it;
    /// it never gates admission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_pressure: Vec<AdmissionCrewPressure>,
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
            || self.usage_pressure.iter().any(|entry| blank(&entry.crew))
    }

    /// `crew`'s reported weekly usage pressure in milli-units, or neutral
    /// (1000 = 1.0) when this capability reports none for it [ORB-17773].
    #[must_use]
    pub fn pressure_milli_for(&self, crew: &str) -> u64 {
        self.usage_pressure
            .iter()
            .find(|entry| entry.crew == crew)
            .map_or(orbit_types::task::NEUTRAL_PRESSURE_MILLI, |entry| {
                entry.pressure_milli
            })
    }

    /// Whether this capability reports any usage pressure at all.
    #[must_use]
    pub fn has_pressure(&self) -> bool {
        !self.usage_pressure.is_empty()
    }
}

/// One kind of non-template work an executor's host has room for: a work
/// class and the complexity its tasks carry.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct AdmissionWorkKey {
    /// `no-diff` or `change`.
    pub work_class: String,
    /// The task's complexity as its tasks spell it (`low`, `xhard`,
    /// `unassessed`); absent for tasks with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
}

/// The work a pulling executor's host load forecast has room for, for the
/// whole horizon of its resource profile. The owner admits only a task inside
/// it: a recurring `auto-task:` task when its template is listed, or when the
/// executor has no profile of its template and `unprofiled_template_classes`
/// lists its base class; any other task when its work class and complexity
/// are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionWorkFit {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<AdmissionWorkKey>,
    /// Templates the executor has a profile of that fit, without the
    /// `auto-task:` prefix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<String>,
    /// Templates the executor has a profile of that do not fit, without the
    /// `auto-task:` prefix. Every template the executor has a profile of is
    /// in `templates` or here, so the owner can tell a template it has never
    /// run from one with no room.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unfit_templates: Vec<String>,
    /// The base classes, `no-diff` or `change`, under which a template the
    /// executor has no profile of fits, priced as the executor would reserve
    /// it: by the fallback chain from that base class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unprofiled_template_classes: Vec<String>,
}

impl AdmissionWorkFit {
    /// Whether nothing fits, so a request carrying it could admit nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
            && self.templates.is_empty()
            && self.unprofiled_template_classes.is_empty()
    }

    /// Why `task` falls outside this fit, or `None` when it is inside it.
    #[must_use]
    pub fn unfit_reason(&self, task: &orbit_types::task::Task) -> Option<String> {
        if let Some(template) = orbit_types::task::auto_task_template(&task.tags) {
            let listed = |names: &[String]| names.iter().any(|name| name == template);
            if listed(&self.templates) {
                return None;
            }
            if listed(&self.unfit_templates) {
                return Some(format!(
                    "the executor's host load forecast has no room for template `{template}`"
                ));
            }
            let base = if task
                .tags
                .iter()
                .any(|tag| tag == orbit_types::task::NO_DIFF_EXPECTED_TAG)
            {
                orbit_types::task::NO_DIFF_WORK_CLASS
            } else {
                orbit_types::task::CHANGE_WORK_CLASS
            };
            return (!self
                .unprofiled_template_classes
                .iter()
                .any(|class| class == base))
            .then(|| {
                format!(
                    "the executor's host load forecast has no room for template `{template}`, \
                     which it has no profile of, as {base} work"
                )
            });
        }
        let key = AdmissionWorkKey {
            work_class: orbit_types::task::work_class(&task.tags),
            complexity: task
                .complexity
                .map(|complexity| complexity.as_str().to_string()),
        };
        (!self.keys.contains(&key)).then(|| {
            format!(
                "the executor's host load forecast has no room for {} work of complexity {}",
                key.work_class,
                key.complexity.as_deref().unwrap_or("unset")
            )
        })
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
    /// Whether the executor's captured `review.timing` is `before_pr` [ORB-13992]. Diagnostic
    /// only: a claimed leaf runs the review the ship contract captured, never
    /// the executor's own [ORB-13908]. Requests recorded before revision 5
    /// carried a `caller_review_policy`, which reads as `false`.
    #[serde(default)]
    pub caller_before_pr: bool,
    /// Whether the executor's claimed leaf runs the before-PR gate the ship
    /// contract's `review` captures [ORB-13895]. An owner with
    /// `review.timing = before_pr` admits only an executor that declares it.
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
    /// Whether the executor's host has a verified headless browser kit, which
    /// a candidate tagged `ui` needs. A request without the field (one
    /// recorded before it existed, or from an executor with no kit) declares
    /// none, so no `ui` task is admitted to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser_kit: bool,
    /// The executor's `machine.labels`, normalized. Its `os` counts as an
    /// implicit label beside them. A candidate whose `requires_labels` are
    /// not all among them is skipped. Part of the request schema, so a peer
    /// whose build predates labels fails the fingerprint check and is
    /// refused rather than admitted to labelled work it would ignore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// The work the executor's host load forecast has room for, under
    /// `machine.auto_concurrency.mode = "enforce"`. Absent otherwise, and from
    /// an executor with no resource profiles: then every work class and
    /// complexity is admissible, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admissible: Option<AdmissionWorkFit>,
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
    /// How `crew` was assigned (`explicit`, `pool:<tier>`,
    /// `purpose:<purpose>`, `default`), so a follower can tell an
    /// automatic assignment it may re-draw under its own usage pressure
    /// from an explicit pin it must keep [ORB-17773]. Absent on receipts
    /// from an older owner and on legacy tasks with no recorded source:
    /// both read as not automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_source: Option<String>,
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
    /// Ready candidates tagged `ui` skipped because the executor declared no
    /// verified browser kit ([`AdmissionRequest::browser_kit`]). They stay
    /// in the backlog for a host that has one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_kit_unavailable: Vec<AdmissionDiagnostic>,
    /// Ready candidates skipped because the executor lacks one of their
    /// `requires_labels` ([`AdmissionRequest::labels`] plus its OS). They
    /// stay in the backlog for a host that carries them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels_unavailable: Vec<AdmissionDiagnostic>,
    /// Ready candidates skipped because their work is outside the request's
    /// [`AdmissionRequest::admissible`] fit. They stay in the backlog until
    /// the executor's host has room, or for another host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capacity_unavailable: Vec<AdmissionDiagnostic>,
    pub queue_depth: usize,
    /// The owner's `workflow.required_validation_commands` and
    /// `review.baseline_commands` when it admitted this request
    /// [ORB-15622]. The claimed leaf validates, accepts sandbox deferrals and
    /// replays them against these, never its own configuration. A response
    /// field outside the pull request's fingerprint: an older follower
    /// ignores it, and a receipt from an older owner carries none, so its
    /// leaf falls back to the replica's configuration with a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gates: Option<orbit_types::workflow::ValidationGates>,
    /// Every tag instruction the owner applies, resolved when it admitted
    /// this request. The claimed leaf's activities receive these, never its
    /// replica's configuration or files, so one task gets the same guidance
    /// on every host. Like `gates`, a response field outside the request's
    /// fingerprint: a receipt from an older owner carries none, and its leaf
    /// falls back to resolving on the replica with a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_instructions: Option<orbit_types::workflow::CapturedTagInstructions>,
    /// [ORB-17304] How the executor census the owner ordered against classes
    /// the admitted task, and the census's machine ids. Absent from an idle
    /// or repair receipt, from an order taken without a census, and from a
    /// receipt of an older owner. A response field outside the request's
    /// fingerprint, like `gates`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity: Option<super::AdmissionAffinity>,
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

/// One task an owner-local drain wave selected, with the canonical lock
/// footprint its gate will reserve [ORB-15514].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerWaveCandidate {
    pub task_id: String,
    pub footprint: Vec<String>,
}

/// What the owner wave's admission section decided for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerWaveAdmission {
    pub task_id: String,
    /// The live execution claim that holds the task or an overlapping
    /// footprint, or `None` once the candidate is admitted and its footprint
    /// is visible to every later pull grant.
    pub blocked_by: Option<Box<ExecutionClaim>>,
}

/// A stopped landing's repair awaiting its one automatic repair leaf, as the
/// owner's local drain sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRepair {
    pub task_id: String,
    pub claim_id: String,
    /// The executor of the stopped delivery, which the repair waits for.
    pub original_executor: ExecutionLocation,
    /// Why the owner may not take the repair yet, naming that executor and
    /// when the owner may, or `None` once the owner may take it.
    pub wait: Option<String>,
}

impl OwnerWaveAdmission {
    pub fn admitted(task_id: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
            blocked_by: None,
        }
    }
}
