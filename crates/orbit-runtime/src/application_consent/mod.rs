//! Application consent validation, host authority and constrained call admission.

mod admission;
mod authority;
mod invocation;
mod validation;

pub use invocation::{finish_application_dispatch, take_application_audit};
