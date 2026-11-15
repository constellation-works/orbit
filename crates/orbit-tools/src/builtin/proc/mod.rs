mod git_config;
mod rustup_install;
pub mod spawn;

#[cfg(test)]
mod tests;

use crate::ToolRegistry;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(spawn::ProcSpawnTool);
}
