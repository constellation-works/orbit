//! Host plugins: the manifest-derived contributions, recorded grants,
//! install layout, and the load pass that registers them on the runtime.

pub mod backend;
#[cfg(unix)]
pub mod broker;
pub mod build_witness;
pub mod cache;
pub mod config;
pub mod definitions;
pub mod discovery;
pub mod grants;
pub mod host;
pub mod paths;
pub mod requirements;
pub mod sandbox_mask;
pub mod secrets;

#[cfg(test)]
mod tests;
