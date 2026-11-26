use clap::Args;
use orbit_core::Core;
use orbit_core::app::skill::{Command, Outcome, UnlinkResult, exec};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct SkillUnlinkArgs {}

impl ExecuteCore for SkillUnlinkArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Unlinked(result) = exec(core, Command::Unlink)? else {
            unreachable!("skill unlink returned an unrelated outcome");
        };
        let mut lines = Vec::new();
        if result.removed_count == 0 {
            lines.push("No skill symlinks found to remove.".to_string());
        } else {
            lines.push(format!(
                "Removed {} skill symlink(s).",
                result.removed_count
            ));
        }
        if !result.cleaned_dirs.is_empty() {
            lines.push("Cleaned up empty directories:".to_string());
            for dir in &result.cleaned_dirs {
                lines.push(format!("  {}", dir.display()));
            }
        }
        Ok(Payload::detail(unlink_result_json(&result), lines.join("\n")).into())
    }
}

fn unlink_result_json(result: &UnlinkResult) -> Value {
    json!({
        "removed_count": result.removed_count,
        "cleaned_dirs": result.cleaned_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    })
}
