//! Built-in tool implementations (`github`, `orbit`, `proc`) registered into `ToolRegistry`; imports crate-root leaves, `orbit-common`, `orbit-exec`, and `orbit-types`.

pub mod github;
pub mod orbit;
pub mod proc;

use crate::ToolRegistry;

pub fn register_builtins(registry: &mut ToolRegistry) {
    github::register(registry);
    orbit::register(registry);
    proc::register(registry);
}
