//! `orbit clock` — inspect, control, and manually tick the host scheduler.

mod command;
pub(crate) mod tick;

pub use command::{ClockCommand, ClockSubcommand};
pub use tick::ClockTickArgs;
