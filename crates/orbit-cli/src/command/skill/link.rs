use clap::Args;
use orbit_core::Core;
use orbit_core::app::skill::{Command, LinkResult, Outcome, exec};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct SkillLinkArgs {}

impl ExecuteCore for SkillLinkArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Linked(result) = exec(core, Command::Link)? else {
            unreachable!("skill link returned an unrelated outcome");
        };
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
