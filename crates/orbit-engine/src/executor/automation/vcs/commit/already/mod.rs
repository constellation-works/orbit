//! Completion on a base that already satisfies the task. `landed` revalidates
//! an existing delivery, and `resolved` validates a claimed implementer's
//! already-resolved checkpoint on the clean base. The distinct acceptance
//! schemas, the generic clean-tree verifier and the no-diff routes stay
//! outside this directory.

pub(super) mod landed;
pub(super) mod resolved;
