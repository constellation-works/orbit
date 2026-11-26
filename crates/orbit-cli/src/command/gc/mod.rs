//! Out-of-band garbage collection for resources left behind by terminal runs.

mod audit;
mod command;
mod deployments;
mod journal;
mod runs;
mod support;
mod tmp;
mod worktrees;

pub use command::{GcCommand, GcTarget};
