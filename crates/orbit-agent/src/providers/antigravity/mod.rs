mod antigravity_cli;
mod antigravity_output;
mod antigravity_runtime;

pub use antigravity_cli::apply_antigravity_print_timeout;
pub use antigravity_output::antigravity_terminal_error_diagnostic;
pub(crate) use antigravity_output::normalize_antigravity_stdout;
pub(crate) use antigravity_runtime::AntigravityFactory;

#[cfg(test)]
mod tests;
