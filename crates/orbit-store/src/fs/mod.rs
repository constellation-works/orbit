//! Narrow filesystem mechanics shared by persistence drivers and workflows.

pub(crate) mod lock;
pub(crate) mod path_safety;
pub(crate) mod yaml;

#[cfg(test)]
mod tests;

pub(crate) mod routine_sweep_lock;
