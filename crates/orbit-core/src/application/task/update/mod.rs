mod artifacts;
mod entry;
mod fields;
mod locked;

pub(crate) use artifacts::admit_agent_artifact;
pub(in crate::application::task) use entry::task_field_writer;
