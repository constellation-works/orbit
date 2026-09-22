mod add;
mod command;
mod disable;
mod doctor;
mod enable;
mod list;
mod migrate;
mod remove;
mod scaffold;
mod show;
mod support;
mod sync;
mod test;
mod upgrade;
mod validate;

pub use command::{PluginCommand, PluginSubcommand};

#[cfg(test)]
mod tests;
