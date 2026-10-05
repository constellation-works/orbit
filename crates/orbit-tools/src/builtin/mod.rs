pub mod github;
pub mod orbit;
pub mod proc;

use crate::ToolRegistry;

pub fn register_builtins(registry: &mut ToolRegistry) {
    github::register(registry);
    orbit::register(registry);
    proc::register(registry);
}
