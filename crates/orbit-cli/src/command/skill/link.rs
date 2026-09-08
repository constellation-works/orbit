use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::bootstrap::init::LinkResult;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct SkillLinkArgs {
    #[arg(long)]
    pub json: bool,
}

impl Execute for SkillLinkArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = orbit_core::bootstrap::init::link_skills(&runtime.global_root())?;
        let text = if result.linked_count == 0 {
            "Skill symlinks are already up to date.".to_string()
        } else {
            let mut lines = vec![format!("Linked {} skill(s) in:", result.linked_count)];
            for root in &result.roots {
                lines.push(format!("  {}", root.display()));
            }
            lines.join("\n")
        };
        Ok(Payload::detail(link_result_json(&result), text).into())
    }
}

fn link_result_json(result: &LinkResult) -> Value {
    json!({
        "linked_count": result.linked_count,
        "roots": result.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
}
