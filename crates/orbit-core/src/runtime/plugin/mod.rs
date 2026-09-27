//! Host plugins: the manifest-derived contributions, recorded grants,
//! install layout, and the load pass that registers them on the runtime.

pub(crate) mod backend;
#[cfg(unix)]
pub(crate) mod broker;
pub(crate) mod cache;
pub(crate) mod config;
pub(crate) mod definitions;
pub mod discovery;
pub mod grants;
pub mod host;
pub mod paths;
pub mod requirements;
pub mod sandbox_mask;
pub mod secrets;

#[cfg(test)]
mod tests;
