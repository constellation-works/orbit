mod git_config;
mod rustup_install;
pub mod spawn;

use crate::ToolRegistry;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(spawn::ProcSpawnTool);
}
