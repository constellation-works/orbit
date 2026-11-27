//! The lease app: path-lease operations (`request`, `renew`, `expand`,
//! `release`, `show`, `list`, `check`, `notices`) and live notice feeds.

mod flow;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{AuthorizationDenial, DASHBOARD_LEASE_WRITE};
use serde_json::Value;

use super::exec::{self, App, Operation};
use crate::Core;
pub use flow::LeaseNoticeFeed;

/// A dashboard lease write verb admitted before opening the selected workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseWrite {
    /// Request a path lease.
    Request,
    /// Renew an active path lease.
    Renew,
    /// Expand an active path lease with additional selectors.
    Expand,
    /// Release a path lease.
    Release,
}

/// Input for a dashboard lease write whose operator session has already been
/// admitted via [`Command::write`].
pub struct AdmittedWrite {
    input: Value,
}

/// What the lease app can be asked to do: one variant per lease verb, plus
/// opening a live notice feed.
pub enum Command {
    /// Request a path lease.
    Request(AdmittedWrite),
    /// Renew an active path lease.
    Renew(AdmittedWrite),
    /// Expand an active path lease with additional selectors.
    Expand(AdmittedWrite),
    /// Release a path lease.
    Release(AdmittedWrite),
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
}

impl Command {
    /// Admit a dashboard lease write before opening the selected workspace.
    pub fn write(
        action: LeaseWrite,
        input: Value,
        operator: bool,
    ) -> Result<Self, AuthorizationDenial> {
        exec::dashboard_authorize(&DASHBOARD_LEASE_WRITE, operator)?;
        let write = AdmittedWrite { input };
        Ok(match action {
            LeaseWrite::Request => Self::Request(write),
            LeaseWrite::Renew => Self::Renew(write),
            LeaseWrite::Expand => Self::Expand(write),
            LeaseWrite::Release => Self::Release(write),
        })
    }
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
        };
        Operation::command("lease", subcommand)
    }

    fn run(core: &Core, command: Self::Command) -> Result<Self::Outcome, Self::Error> {
        match command {
            Command::Request(write) => flow::request(core, write.input).map(Outcome::Value),
            Command::Renew(write) => flow::renew(core, write.input).map(Outcome::Value),
            Command::Expand(write) => flow::expand(core, write.input).map(Outcome::Value),
            Command::Release(write) => flow::release(core, write.input).map(Outcome::Value),
            Command::Show(input) => flow::show(core, input).map(Outcome::Value),
            Command::List => flow::list(core).map(Outcome::Value),
            Command::Check(input) => flow::check(core, input).map(Outcome::Value),
            Command::Notices(input) => flow::notices(core, input).map(Outcome::Value),
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
