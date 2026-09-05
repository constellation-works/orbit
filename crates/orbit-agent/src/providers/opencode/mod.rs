mod opencode_cli;
mod opencode_output;
mod opencode_runtime;

pub(crate) use opencode_output::normalize_opencode_stdout;
pub(crate) use opencode_runtime::OpencodeFactory;

#[cfg(test)]
mod tests;
