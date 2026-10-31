//! Owner-served mutating half of the distributed drain [ORB-13625]: pull
//! admission, run binding and claim settlement.
//!
//! # Authority
//!
//! These are the entry points a follower's drain reaches over federated MCP,
//! so every authority fact is read from the trusted session the transport
//! built, exactly as the read-only half does:
//!
//! - **Access** is SSH login plus the `agent` or `operator` capability the
//!   governed-operation rows require [ORB-12564]. There is no callers file.
//! - **Attempt ownership** is the claim journal's own fence. Every mutation
//!   here reaches it as a [`orbit_store::contracts::ClaimInvocation`] whose machine comes from the
//!   session, never tool input. For SSH MCP that machine is the caller-chosen
//!   remote label; for local sessions it is the accepting machine's identity.
//!   The journal compares it with the admitted execution machine, bound run
//!   and phase. This prevents mixed attempts among cooperating executors, not
//!   impersonation by someone able to start a server with another label:
//!   local account access and SSH login are already trusted owner access.
//! - **Observations** are the owner's own. A handoff payload names the
//!   candidate to look at; the owner reads the published pull request from the
//!   provider and resolves both commits in its own checkout before the journal
//!   compares them.
//!
//! A remote executor is refused local ship mode by the admission ladder and a
//! local-candidate handoff here: followers never run owner-local leaves, so
//! only the owner may hand off a candidate that exists solely in its checkout.

mod admission;
mod binding;
mod handoff;
mod identity;
mod known_red;
mod model;
mod pull;
mod refusal;
mod settlement;

pub use model::TaskPullResponse;
