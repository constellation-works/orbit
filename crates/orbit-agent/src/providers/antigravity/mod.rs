mod antigravity_cli;
mod antigravity_output;
mod antigravity_runtime;

pub(crate) use antigravity_output::normalize_antigravity_stdout;
pub(crate) use antigravity_runtime::AntigravityFactory;

#[cfg(test)]
mod tests;
