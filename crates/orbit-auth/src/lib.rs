//! Orbit's credential code: lease tokens for the lease listener user.
//!
//! One crate owns every credential mechanism so one place carries the
//! security review: the token format, generation, hashing and constant-time
//! verification ([`token`]); principals, classes, verbs, scopes and the
//! allow-or-deny decision ([`scope`]); the listener user's token store
//! ([`store`]); and the execution-environment check every listener-user
//! command runs first ([`environment`]). It has no HTTP framework, runtime
//! handle or Orbit store; surfaces wrap it.

pub mod environment;
pub mod scope;
pub mod store;
pub mod token;
