mod add;
mod command;
mod disable;
mod doctor;
mod enable;
mod list;
mod migrate;
mod remove;
mod show;
mod support;
mod sync;
mod validate;

pub use command::{PluginCommand, PluginSubcommand};

#[cfg(test)]
mod tests;
