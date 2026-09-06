pub(crate) mod copilot_cli;
mod copilot_runtime;
mod copilot_stream;

pub(crate) use copilot_runtime::CopilotFactory;
pub(crate) use copilot_stream::{normalize_copilot_stdout, project_copilot_response};

#[cfg(test)]
mod tests;
