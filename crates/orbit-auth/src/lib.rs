//! Orbit's credential code: lease tokens for the lease listener user.
//!
//! One crate owns every credential mechanism so one place carries the
//! security review: the token format, generation, hashing and constant-time
//! verification ([`token`]); principals, classes, verbs, scopes and the
//! allow-or-deny decision ([`scope`]); the listener user's token store
//! ([`store`]); the execution-environment check every listener-user
//! command runs first ([`environment`]); the listener's client-address rule
//! ([`client_address`]); and its failed-authentication limiter
//! ([`limiter`]). It has no HTTP framework, runtime handle or Orbit store;
//! surfaces wrap it.

pub mod client_address;
pub mod environment;
pub mod limiter;
pub mod scope;
pub mod store;
pub mod token;
