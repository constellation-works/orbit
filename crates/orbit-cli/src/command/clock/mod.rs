//! `orbit clock` — inspect, control, and manually tick the host scheduler.

mod command;
mod repair;
pub(crate) mod tick;

pub use command::{ClockCommand, ClockSubcommand};
pub use tick::ClockTickArgs;
