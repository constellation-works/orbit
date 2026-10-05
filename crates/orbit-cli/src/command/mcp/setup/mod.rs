mod args;
mod dispatch;
mod format;
mod providers;
mod workspace;

pub(crate) use args::init_auto_for_workspace;
pub use args::{InitArgs, RemoveArgs};
pub(crate) use dispatch::registered_clients_for_workspace;
