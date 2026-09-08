//! Initialization, managed asset seeding, and forward-only startup migrations.

pub(crate) mod activity;
pub(crate) mod global_defaults;
pub mod init;
pub(crate) mod policy;
pub mod task_migration;
pub mod task_publication;

#[cfg(test)]
mod tests;
