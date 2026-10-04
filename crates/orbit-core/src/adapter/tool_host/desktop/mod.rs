//! Desktop tool adapters: task snapshot/write and read forwarding, bounded
//! reads, automation and drain controls.
pub(super) mod automation;
pub(super) mod drain;
mod read;
pub(super) mod task;
