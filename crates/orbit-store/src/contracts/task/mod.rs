//! Task contracts: the commit boundary (`coordination`), request-scoped
//! selection and response rows (`query`), and the task registry's bindings,
//! index rows and allocator (`registry`).

mod coordination;
mod query;
mod registry;

pub use coordination::*;
pub use query::*;
pub use registry::*;
