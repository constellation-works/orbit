//! The review app: distributed claim provenance, owner handoff actions and
//! operator consent for scoped applications.
//!
//! Neither noun is in the operation registry, so [`Command`] is written by
//! hand: the command line's `application` consent/rotate/revoke verbs, the
//! dashboard's read-only claim console and its governed owner handoff and
//! claim recovery actions.

mod dashboard;

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::tool::ApplicationConsent;
use orbit_types::workflow::handoff::LandingAttemptState;
use serde_json::Value;

use super::exec::{self, App, DashboardOrigin, DashboardRow, Operation};
use crate::Core;
pub use crate::application::operator_input::replay_key;
pub use crate::application::review::{
    DistributedClaimState, ExpectedCandidate, HandoffConsoleRefusal,
};

/// What the review app can be asked to do.
pub enum Command {
    /// Approve a consent request file and provision a new private credential
    /// file, as `orbit application consent` does.
    ConsentApplication {
        /// JSON consent request path.
        input_file: PathBuf,
        /// New owner-only path receiving the credential once.
        credential_file: PathBuf,
    },
    /// Revoke an existing consent and provision a replacement credential, as
    /// `orbit application rotate` does.
    RotateApplication {
        /// Existing consent id to revoke.
        consent_id: String,
        /// JSON consent request path.
        input_file: PathBuf,
        /// New owner-only path receiving the credential once.
        credential_file: PathBuf,
    },
    /// Revoke an application consent and all callbacks minted from it, as
    /// `orbit application revoke` does.
    RevokeApplication {
        /// Consent id to revoke.
        consent_id: String,
    },
    /// Read-only owner claim provenance console for the dashboard.
    ClaimConsole {
        /// Optional task id filter.
        task_id: Option<String>,
        /// Lifecycle state filter.
        state: DistributedClaimState,
        /// Whether settled claims expand to full detail.
        detail: bool,
    },
    /// One governed owner handoff or claim action from an authenticated
    /// dashboard session.
    Dashboard(DashboardOrigin, DashboardAction),
}

/// One owner handoff or claim action sent from the dashboard, with its
/// optional forwarding operator session identity.
pub struct DashboardAction {
    /// Forwarding operator session identity, if any.
    pub write_caller: Option<String>,
    /// The handoff or claim mutation to run.
    pub write: DashboardWrite,
}

/// One owner handoff or claim mutation sent from the dashboard.
#[derive(Clone)]
pub enum DashboardWrite {
    /// Record completion authority for one exact candidate.
    ApproveHandoff {
        /// Handoff id to approve.
        handoff_id: String,
        /// Candidate and base commit the operator observed.
        expected: ExpectedCandidate,
        /// Normalized replay key for this approval decision.
        request_id: String,
    },
    /// Retry a stopped or uncertain authorized owner landing.
    RetryHandoff {
        /// Handoff id to retry.
        handoff_id: String,
        /// Candidate commit the operator observed.
        expected_candidate_commit: String,
        /// Base commit the operator observed.
        expected_base_commit: String,
        /// Latest attempt number the operator observed.
        expected_latest_attempt: u32,
        /// Latest attempt state the operator observed.
        expected_latest_state: LandingAttemptState,
        /// Raw operator reason, normalized before auditing and execution.
        reason: String,
        /// Raw replay key, normalized before auditing and execution.
        request_key: String,
    },
    /// Withdraw completion authority for one exact candidate.
    RevokeHandoff {
        /// Handoff id to revoke.
        handoff_id: String,
        /// Candidate and base commit the operator observed.
        expected: ExpectedCandidate,
        /// Raw operator reason, normalized before auditing and execution.
        reason: String,
        /// Raw replay key, normalized before auditing and execution.
        request_id: String,
    },
    /// Deliberately recover one execution claim and move its task.
    RecoverClaim {
        /// Claim id to recover.
        claim_id: String,
        /// Phase the operator observed.
        expected_phase: String,
        /// Parsed target task status.
        status: TaskStatus,
        /// Raw status label from the request, recorded on the audit row.
        status_label: String,
        /// Raw operator reason, normalized before auditing and execution.
        reason: String,
        /// Raw replay key, normalized before auditing and execution.
        request_id: String,
    },
}

/// Outcome of an admitted owner handoff or claim dashboard action.
pub enum DashboardWritten {
    /// The owner transaction succeeded and produced its JSON payload.
    Applied(Value),
    /// The owner refused the action with a typed console refusal.
    Refused {
        /// Why the owner refused the action.
        refusal: HandoffConsoleRefusal,
        /// The error message to surface verbatim.
        message: String,
    },
    /// The action failed without a typed console refusal.
    Failed(OrbitError),
}

/// What a review command produced.
pub enum Outcome {
    /// Application consent issued or rotated.
    ApplicationConsented(Box<ApplicationConsent>),
    /// Application consent revoked.
    ApplicationRevoked {
        /// The revoked consent id.
        consent_id: String,
    },
    /// Read-only owner claim console payload.
    ClaimConsole(Value),
    /// One dashboard owner action: what it did, or why the session was refused.
    Dashboard(DashboardWritten),
}

/// Why a review command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flow refused or failed the command.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct ReviewApp;

impl App for ReviewApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        // Consent verbs are audited under the admin role the command line
        // declares, not the runtime actor the app guard would otherwise record.
        match command {
            Command::ConsentApplication { .. } => Operation::command("application", "consent")
                .target(Some("application_consent"), None)
                .audit_role("admin"),
            Command::RotateApplication { consent_id, .. } => {
                Operation::command("application", "rotate")
                    .target(Some("application_consent"), Some(consent_id.clone()))
                    .audit_role("admin")
            }
            Command::RevokeApplication { consent_id } => {
                Operation::command("application", "revoke")
                    .target(Some("application_consent"), Some(consent_id.clone()))
                    .audit_role("admin")
            }
            Command::ClaimConsole { .. } => Operation::dashboard_read("review", "claim-console"),
            Command::Dashboard(origin, action) => dashboard::operation(origin, &action.write),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::ConsentApplication {
                input_file,
                credential_file,
            } => Outcome::ApplicationConsented(Box::new(runtime.provision_application_from_file(
                &input_file,
                &credential_file,
                None,
            )?)),
            Command::RotateApplication {
                consent_id,
                input_file,
                credential_file,
            } => Outcome::ApplicationConsented(Box::new(runtime.provision_application_from_file(
                &input_file,
                &credential_file,
                Some(&consent_id),
            )?)),
            Command::RevokeApplication { consent_id } => {
                runtime.revoke_application(&consent_id, &Default::default())?;
                Outcome::ApplicationRevoked { consent_id }
            }
            Command::ClaimConsole {
                task_id,
                state,
                detail,
            } => Outcome::ClaimConsole(runtime.distributed_claim_console_filtered(
                task_id.as_deref(),
                state,
                detail,
            )?),
            Command::Dashboard(_, action) => Outcome::Dashboard(dashboard::write(core, action)?),
        })
    }

    fn dashboard_row(
        result: &Result<Outcome, crate::Error>,
        _target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        let written = match result {
            Ok(Outcome::Dashboard(written)) => Ok(written),
            Ok(_) => return None,
            Err(error) => Err(error),
        };
        dashboard::row(written, arguments)
    }
}

/// Run one review command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<ReviewApp>(core, command)
}
