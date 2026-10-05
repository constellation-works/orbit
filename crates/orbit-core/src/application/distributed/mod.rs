//! Owner-side read-only surface of the distributed drain: the admission
//! preflight probe ([design §4.1]) and receipt reconciliation ([spec]).
//!
//! # Authority
//!
//! SSH login establishes owner access [ORB-12564]. There is no destination
//! callers file, forced-command acceptance, key-bound proof, or replacement
//! identity registry. What still decides a call here is the capability the
//! destination serves this caller — `agent` or `operator` — plus the trusted
//! runtime facts this module reads. The capability is resolved by the tool
//! chokepoint and nowhere else: both read-only tools carry a governed row
//! whose allowed set is `agent` or `operator`, so a caller the chokepoint
//! cannot identify is refused before any function here runs, and a CLI caller
//! — whose authority lives in the process envelope rather than in the session
//! `orbit tool run` builds — is admitted on the same terms as an MCP one
//! [ORB-12582]. A machine label forwarded by an SSH proxy or the federated mux
//! is attribution: it names a receipt namespace and appears in diagnostics,
//! and it never adds a capability the caller did not already hold.
//!
//! # The mutating half
//!
//! Nothing in the read-only surface creates an admission receipt, a claim, a
//! reservation, or a task transition, and nothing here grants execution
//! authority. The mutating entry points a follower's drain needs — pull, run
//! binding and settlement — live in [`serve`] [ORB-13625]. They still name
//! [`ensure_distributed_mutation_available`], so turning the whole feature off
//! again is one source change. Completion approval, revocation and recovery
//! are not among them: those stay owner-operator actions on the dashboard.
//!
//! # The retained entry points
//!
//! This module also owns [`OrbitRuntime::drain_entry_admission`] [ORB-12500]:
//! the one decision an explicit ship, an explicit owner drain and the
//! independent registry-driven ship sweep all take before they dispatch
//! anything. It is here rather than beside any one of them because its whole
//! purpose is that none of the three keeps a private idea of what the host is
//! already doing.
//!
//! [design §4.1]: ../../../../../docs/design/distributed-drain/2_design.md
//! [spec]: ../../../../../docs/design/distributed-drain/specs/task-pull.md

mod contract;
mod entry;
mod final_recovery;
mod follower;
mod probe;
mod serve;
mod settlement;

pub(crate) use contract::protocol_mismatch;
pub use contract::{
    DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED, DeclaredCallerContract, OWNER_COMPLETION_POLICY,
    ensure_distributed_mutation_available, owner_binary_version,
};
pub use entry::{DrainEntryAdmission, DrainEntryPoint, DrainEntryRefusal, RESOURCE_THROTTLED};
pub use follower::{PULL_DRAIN_JOB, WorkspacePullRequest};
pub use probe::{AdmissionReceiptLookup, DrainProbeReport, DrainProbeSession};
pub use serve::TaskPullResponse;
pub use settlement::{
    DrainClaimedLeaf, PendingPullSettlements, PullCrewWindow, PullLeafClaim, PullSettlementEntry,
    RefusedPullSettlement,
};
pub(crate) use settlement::{
    is_owner_refusal, is_owner_transport_failure, settlement_refusal_backoff,
};
