//! Automation records in the existing host SQLite database.

mod backend;
mod checkpoint;
mod codec;
mod intents;
mod members;
mod recovery;
mod schema;
mod transition;
mod waivers;

pub(crate) use schema::{FEATURE, MIGRATIONS, initialize};
