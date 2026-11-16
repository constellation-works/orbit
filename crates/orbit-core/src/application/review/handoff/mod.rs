//! Owner-domain handoff seam. Public distributed mutation tools remain gated.
//! Trusted callers obtain observations from Git/provider state and repository check
//! policy, including the existing already-landed verifier for no-diff work. They
//! must never manufacture observations by copying the worker's handoff payload.

mod accept;
mod candidate;
mod console;
mod console_json;
mod repair;

pub use candidate::ExpectedCandidate;
pub use console::{DistributedClaimState, HandoffConsoleRefusal};
