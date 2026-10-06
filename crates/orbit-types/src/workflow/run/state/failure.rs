//! Failure markers and claim failure classification.

use serde::{Deserialize, Serialize};

/// Token a provider failure diagnostic carries when the provider itself could
/// not be used on this host — its CLI refused authentication, say — as
/// opposed to the agent failing the work [ORB-13941].
///
/// The CLI runner stamps it, bracketed, into the step's failure message; a
/// pull drain reads it back off the terminal leaf to release the claim and
/// exclude the crew for its window instead of failing the owner's task.
pub const PROVIDER_UNAVAILABLE_ERROR_CODE: &str = "provider_unavailable";

/// The bracketed marker form of [`PROVIDER_UNAVAILABLE_ERROR_CODE`].
pub const PROVIDER_UNAVAILABLE_MARKER: &str = "[provider_unavailable]";

/// Whether a step failure says its provider could not be used on this host.
///
/// A [provider capacity](PROVIDER_CAPACITY_ERROR_CODE) failure is one kind of
/// unavailability, so this is true for it too.
#[must_use]
pub fn is_provider_unavailable(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(PROVIDER_UNAVAILABLE_ERROR_CODE)
        || message.is_some_and(|message| message.contains(PROVIDER_UNAVAILABLE_MARKER))
        || is_provider_capacity_exhausted(error_code, message)
}

/// Token a provider failure diagnostic carries when the provider itself said
/// the selected model has no capacity — Codex's `Selected model is at
/// capacity` — as opposed to the agent failing the work [ORB-14149].
///
/// It is a kind of [`PROVIDER_UNAVAILABLE_ERROR_CODE`]: step recovery skips it
/// and a pull drain releases the claim and excludes the crew for its window.
/// Final recovery skips it as well, since no decision about the task changes
/// provider capacity, so the failure handoff keeps the candidate for a resume.
pub const PROVIDER_CAPACITY_ERROR_CODE: &str = "provider_capacity";

/// The bracketed marker form of [`PROVIDER_CAPACITY_ERROR_CODE`].
pub const PROVIDER_CAPACITY_MARKER: &str = "[provider_capacity]";

/// Whether a step failure says its provider's selected model was at capacity.
#[must_use]
pub fn is_provider_capacity_exhausted(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(PROVIDER_CAPACITY_ERROR_CODE)
        || message.is_some_and(|message| message.contains(PROVIDER_CAPACITY_MARKER))
}

/// Token a provider failure diagnostic carries when the provider's own
/// content policy refused the turn — Codex's cybersecurity content filter, a
/// Claude refusal — as opposed to the agent failing the work [ORB-14266].
///
/// It is not an unavailability: the provider works, it declined this task's
/// content, so a pull drain does not exclude the crew for its window. Step
/// and final recovery skip it (a repair agent on the same provider is refused
/// the same way), and a local run's task goes back to the backlog with every
/// crew of that provider withheld for a while.
pub const PROVIDER_REFUSAL_ERROR_CODE: &str = "provider_refusal";

/// The bracketed marker form of [`PROVIDER_REFUSAL_ERROR_CODE`].
pub const PROVIDER_REFUSAL_MARKER: &str = "[provider_refusal]";

/// Whether a step failure says its provider's content policy refused the turn.
#[must_use]
pub fn is_provider_refusal(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(PROVIDER_REFUSAL_ERROR_CODE)
        || message.is_some_and(|message| message.contains(PROVIDER_REFUSAL_MARKER))
}

/// Whether a step failure is the provider's rather than the work's: it was
/// [unavailable](is_provider_unavailable) (capacity included) or
/// [refused](is_provider_refusal) the turn.
#[must_use]
pub fn is_provider_failure(error_code: Option<&str>, message: Option<&str>) -> bool {
    is_provider_unavailable(error_code, message) || is_provider_refusal(error_code, message)
}

/// Token a required-validation failure carries when a command could not find
/// a tool in the validation environment — exit 127, `command not found`, or a
/// guardrail's "is required" — as opposed to the candidate failing its checks
/// [ORB-13987].
///
/// No repair of the candidate can install a tool, so step and final recovery
/// skip it, the failure handoff keeps the candidate for `orbit job resume`
/// without opening a `[BLOCKED]` PR, and a pull drain releases the claim.
pub const VALIDATION_ENVIRONMENT_ERROR_CODE: &str = "validation_environment";

/// The bracketed marker form of [`VALIDATION_ENVIRONMENT_ERROR_CODE`].
pub const VALIDATION_ENVIRONMENT_MARKER: &str = "[validation_environment]";

/// Whether a step failure says required validation lacked a tool.
#[must_use]
pub fn is_validation_environment_failure(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(VALIDATION_ENVIRONMENT_ERROR_CODE)
        || message.is_some_and(|message| message.contains(VALIDATION_ENVIRONMENT_MARKER))
}

/// Token a step failure carries when a claimed leaf could not reach its
/// task's owner from where the step ran — the worker's owner route is masked
/// or the owner is unreachable — as opposed to the candidate failing
/// [ORB-14257]. The worker's owner route stamps it on a transport failure.
///
/// Inside the agent sandbox, which masks the SSH credentials, the nested
/// `orbit` raises it when its run's coordinator — the step runner's broker —
/// was not passed to it or had stopped [ORB-14260]. No repair agent can open
/// that route from the same sandbox, so step and final recovery skip it.
pub const OWNER_ROUTE_UNAVAILABLE_ERROR_CODE: &str = "owner_route_unavailable";

/// The bracketed marker form of [`OWNER_ROUTE_UNAVAILABLE_ERROR_CODE`].
pub const OWNER_ROUTE_UNAVAILABLE_MARKER: &str = "[owner_route_unavailable]";

/// Whether a step failure says a claimed worker could not reach its owner.
#[must_use]
pub fn is_owner_route_unavailable(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(OWNER_ROUTE_UNAVAILABLE_ERROR_CODE)
        || message.is_some_and(|message| message.contains(OWNER_ROUTE_UNAVAILABLE_MARKER))
}

/// Token a step failure carries when its outcome was inconclusive for a
/// reason that may not recur [ORB-14257]: claimed required validation stamps
/// it on a command that still could not reach the network after its reruns.
pub const TRANSIENT_FAILURE_ERROR_CODE: &str = "transient_failure";

/// The bracketed marker form of [`TRANSIENT_FAILURE_ERROR_CODE`].
pub const TRANSIENT_FAILURE_MARKER: &str = "[transient_failure]";

/// Why a claimed leaf ended without its typed handoff, as its settlement
/// carries it to the owner [ORB-14257].
///
/// The owner blocks the task only for [`Self::Candidate`] and
/// [`Self::TaskInput`]: the work or the task itself needs a human. Every
/// other class is the executing host's, the base's or the moment's, so the
/// claim is released to the backlog instead, within a per-task release budget
/// that counts every typed release.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ClaimFailureClass {
    /// The candidate failed: its implementation, checks or review.
    Candidate,
    /// The task should not be done as written, or is obsolete.
    TaskInput,
    /// The executing host lacked something the work needs (a validation
    /// tool, a nested sandbox).
    Environment,
    /// Required validation fails on the base as well as on the candidate.
    BaselineRed,
    /// An inconclusive outcome that may not recur, or a leaf whose worker died.
    Transient,
    /// An operator cancelled the launched leaf.
    OperatorCancel,
    /// The leaf could not reach the task's owner from where it ran.
    OwnerRoute,
    /// The crew's provider could not be used on the executing host.
    Provider,
    /// The leaf's committed candidate could not be synchronized onto a base
    /// that moved under it, and conflict recovery did not resolve it. The
    /// candidate is kept for the next claim to carry onto the new base.
    BaseConflict,
}

impl ClaimFailureClass {
    /// The class's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::TaskInput => "task_input",
            Self::Environment => "environment",
            Self::BaselineRed => "baseline_red",
            Self::Transient => "transient",
            Self::OperatorCancel => "operator_cancel",
            Self::OwnerRoute => "owner_route",
            Self::Provider => "provider",
            Self::BaseConflict => "base_conflict",
        }
    }

    /// Whether the owner blocks the task for it rather than releasing it.
    #[must_use]
    pub const fn blocks(self) -> bool {
        matches!(self, Self::Candidate | Self::TaskInput)
    }

    /// Whether a release of this class counts against the task's release
    /// budget. Every typed release counts; the third within the window blocks
    /// the task until a human decides what should change.
    #[must_use]
    pub const fn budgeted(self) -> bool {
        !self.blocks()
    }

    /// Whether the executing drain stops running the leaf's crew for the
    /// rest of its window. A cancel excludes that crew, but does not suppress
    /// the whole host; a red base and a base conflict exclude neither.
    #[must_use]
    pub const fn excludes_crew(self) -> bool {
        matches!(
            self,
            Self::Environment
                | Self::Transient
                | Self::OperatorCancel
                | Self::OwnerRoute
                | Self::Provider
        )
    }

    /// Whether the failure is the executing host's whatever crew runs there —
    /// a validation environment it lacks, an owner it cannot reach — so the
    /// drain stops claiming work on that host for the rest of its window,
    /// and the owner admits nothing more to that drain.
    #[must_use]
    pub const fn suppresses_host(self) -> bool {
        matches!(self, Self::Environment | Self::OwnerRoute)
    }

    /// The class a typed step failure names, or `None` for an untyped one.
    /// A red base counts only when the failure carries its
    /// [hold](super::super::BaselineRedHold), which the owner needs to lift it.
    #[must_use]
    pub fn of_step_failure(error_code: Option<&str>, message: Option<&str>) -> Option<Self> {
        let typed = |code: &str, marker: &str| {
            error_code == Some(code) || message.is_some_and(|message| message.contains(marker))
        };
        if is_provider_unavailable(error_code, message) {
            Some(Self::Provider)
        } else if is_validation_environment_failure(error_code, message) {
            Some(Self::Environment)
        } else if is_owner_route_unavailable(error_code, message) {
            Some(Self::OwnerRoute)
        } else if super::super::is_baseline_red_failure(error_code, message)
            && message
                .and_then(super::super::BaselineRedHold::from_text)
                .is_some()
        {
            Some(Self::BaselineRed)
        } else if typed(TRANSIENT_FAILURE_ERROR_CODE, TRANSIENT_FAILURE_MARKER) {
            Some(Self::Transient)
        } else {
            None
        }
    }
}
