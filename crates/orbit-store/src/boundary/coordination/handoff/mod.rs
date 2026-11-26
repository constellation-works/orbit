//! Typed handoff acceptance and completion authority on the claim journal.

use super::TaskCommitBoundary;

mod accept;
mod authority;
mod evidence;
mod queries;
mod repair;
mod review;

const HANDOFF: &str = "distributed-handoff-v1";
const AUTHORIZATION: &str = "distributed-handoff-authorization-v1";
const REVOCATION: &str = "distributed-handoff-revocation-v1";
const START: &str = "distributed-landing-start-v1";
