//! Provider envelope, environment preparation and subprocess execution.

mod environment;
mod execution;
mod preparation;

pub(super) use execution::dispatch_prepared_provider;
