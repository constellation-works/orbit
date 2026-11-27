//! The lease app: path-lease operations (`request`, `renew`, `expand`,
//! `release`, `show`, `list`, `check`, `notices`) and live notice feeds.

mod flow;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::DASHBOARD_LEASE_WRITE;
use serde_json::Value;

use orbit_types::telemetry::AuditEventStatus;

use super::exec::{self, App, Operation, Session};
use crate::Core;
pub use crate::runtime::stored::locks::lease::{
    LeasePrincipalClass, LeasePriority, LeaseTokenGrant,
};
pub use flow::LeaseNoticeFeed;

/// What the lease app can be asked to do: one variant per lease verb, plus
/// opening a live notice feed.
pub enum Command {
    /// Request a path lease.
    Request(Value),
    /// Renew an active path lease.
    Renew(Value),
    /// Expand an active path lease with additional selectors.
    Expand(Value),
    /// Release a path lease.
    Release(Value),
    /// Show one path lease.
    Show(Value),
    /// List active path leases.
    List,
    /// Check paths against active leases.
    Check(Value),
    /// Read one page of lease notices.
    Notices(Value),
    /// Open a live notice feed and read its first page before sending SSE headers.
    NoticeFeed(Value),
    /// One operation of the intake socket, which a lease token authorized.
    Intake(Intake),
}

/// One operation of the lease intake socket. The socket authenticated the
/// token that authorizes it, so the session is asked for nothing.
pub enum Intake {
    /// Request a path lease within the token's grant.
    Request(Value, LeaseTokenGrant),
    /// Renew a lease the token covers.
    Renew(Value, LeaseTokenGrant),
    /// Expand a lease within the token's grant.
    Expand(Value, LeaseTokenGrant),
    /// Release a path lease.
    Release(Value),
    /// Show one path lease.
    Show(Value),
    /// List active path leases.
    List,
    /// Check paths against active leases.
    Check(Value),
    /// Read one page of lease notices.
    Notices(Value),
    /// Record one intake operation's audit event.
    Audit {
        /// The intake operation's name.
        operation: String,
        /// The lease the operation named or granted.
        lease_id: Option<String>,
        /// How the operation ended.
        status: AuditEventStatus,
        /// The event's payload.
        payload: Value,
    },
}

/// What a lease command produced.
pub enum Outcome {
    /// The JSON contract returned by a single-shot lease operation.
    Value(Value),
    /// The first notice page and the live feed handle for subsequent pages.
    NoticeFeed {
        /// The first page read before sending SSE headers.
        first: Value,
        /// The feed handle scoped to the admitted workspace and query filter.
        feed: Box<LeaseNoticeFeed>,
    },
}

impl Outcome {
    /// Unwrap the JSON contract returned by a single-shot lease operation.
    pub fn into_value(self) -> Result<Value, crate::Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::NoticeFeed { .. } => Err(OrbitError::Execution(
                "lease command returned a notice feed instead of a JSON value".to_string(),
            )),
        }
    }

    /// Unwrap the first page and live feed returned by [`Command::NoticeFeed`].
    pub fn into_notice_feed(self) -> Result<(Value, LeaseNoticeFeed), crate::Error> {
        match self {
            Self::NoticeFeed { first, feed } => Ok((first, *feed)),
            Self::Value(_) => Err(OrbitError::Execution(
                "lease command returned a JSON value instead of a notice feed".to_string(),
            )),
        }
    }
}

/// Why a lease command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Input validation, policy, authorization, or runtime execution failed.
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

struct LeaseApp;

impl App for LeaseApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        let subcommand = match command {
            Command::Request(_) => "request",
            Command::Renew(_) => "renew",
            Command::Expand(_) => "expand",
            Command::Release(_) => "release",
            Command::Show(_) => "show",
            Command::List => "list",
            Command::Check(_) => "check",
            Command::Notices(_) | Command::NoticeFeed(_) => "notices",
            Command::Intake(_) => return Operation::dashboard_read("lease", "intake"),
        };
        let operation = Operation::command("lease", subcommand);
        match command {
            Command::Request(_) | Command::Renew(_) | Command::Expand(_) | Command::Release(_) => {
                operation.requires(&DASHBOARD_LEASE_WRITE)
            }
            Command::Show(_)
            | Command::List
            | Command::Check(_)
            | Command::Notices(_)
            | Command::NoticeFeed(_)
            | Command::Intake(_) => operation,
        }
    }

    fn run(core: &Core, command: Self::Command) -> Result<Self::Outcome, Self::Error> {
        match command {
            Command::Request(input) => flow::request(core, input).map(Outcome::Value),
            Command::Renew(input) => flow::renew(core, input).map(Outcome::Value),
            Command::Expand(input) => flow::expand(core, input).map(Outcome::Value),
            Command::Release(input) => flow::release(core, input).map(Outcome::Value),
            Command::Show(input) => flow::show(core, input).map(Outcome::Value),
            Command::List => flow::list(core).map(Outcome::Value),
            Command::Check(input) => flow::check(core, input).map(Outcome::Value),
            Command::Notices(input) => flow::notices(core, input).map(Outcome::Value),
            Command::Intake(intake) => flow::intake(core, intake).map(Outcome::Value),
            Command::NoticeFeed(input) => {
                flow::notice_feed(core, input).map(|(first, feed)| Outcome::NoticeFeed {
                    first,
                    feed: Box::new(feed),
                })
            }
        }
        .map_err(Error::from)
    }
}

/// Authorize, audit and run one lease command.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<LeaseApp>(core, command)
}

/// Decide what `command` requires of `session` before the selected workspace
/// is opened: a lease write is refused here without one. [`exec`] decides the
/// same requirement again.
pub fn admit(session: Session, command: &Command) -> Result<(), crate::Error> {
    exec::admit::<LeaseApp>(session, command)
}
