mod add;
mod command;
mod disable;
mod doctor;
mod enable;
mod list;
mod migrate;
mod remove;
mod scaffold;
mod secret;
mod show;
mod support;
mod sync;
mod test;
mod upgrade;
mod validate;

pub use command::{PluginCommand, PluginSubcommand};
pub use secret::PluginSecretSubcommand;

#[cfg(test)]
mod tests;
