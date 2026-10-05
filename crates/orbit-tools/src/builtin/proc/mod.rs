mod git_config;
pub mod spawn;

use crate::ToolRegistry;

pub fn register(registry: &mut ToolRegistry) {
    registry.register(spawn::ProcSpawnTool);
}
