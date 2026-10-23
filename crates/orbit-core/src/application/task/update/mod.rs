//! Task update entry points, locked writes, field admission and attachments.

mod artifacts;
mod entry;
mod fields;
mod locked;

pub(crate) use artifacts::admit_agent_artifact;
pub(super) use fields::task_field_writer;
