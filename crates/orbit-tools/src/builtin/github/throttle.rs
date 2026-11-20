//! GitHub's authenticated throttling answer to a read [ORB-20178].
//!
//! Only this adapter types GitHub's rendered diagnostic for a read; the
//! reader itself is the crate-root `gh_request` leaf. It types the one
//! answer a caller's policy may wait out: an authenticated primary or
//! secondary rate limit. Everything else, including an ordinary 403, a 401,
//! a 404 and an unauthenticated limit (which names an address, not an
//! account, so the credential is missing), stays the untyped failure it was.
//! The rendered text is unchanged either way.

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle};
use orbit_types::tool::ExecutionResult;

use crate::check_exec_result;
use crate::gh_request::read_throttle;

/// [`check_exec_result`] for a read, typing GitHub's authenticated throttling
/// answer so the caller can tell it from a refusal.
pub fn check_read_result(result: &ExecutionResult, label: &str) -> Result<(), OrbitError> {
    check_exec_result(result, label).map_err(|error| match read_throttle(&result.stderr) {
        Some(throttle) => OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
            operation: label.to_string(),
            class: PrivateVcsFailureClass::Other,
            message: format!("{label} failed: {}", result.stderr.trim()),
            read_throttle: Some(throttle),
        })),
        None => error,
    })
}

/// The throttle `error` carries, when an adapter typed one.
pub(super) fn throttle_of(error: &OrbitError) -> Option<ProviderReadThrottle> {
    error
        .private_vcs_failure()
        .and_then(|failure| failure.read_throttle.clone())
}
