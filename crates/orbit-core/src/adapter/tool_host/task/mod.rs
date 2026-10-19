//! Task tool handlers, split by handler family: the operation dispatch and
//! reads, the artifact read, and the write verbs.

pub(super) mod operation;
mod read;
mod write;
