use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::bootstrap::init::UnlinkResult;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct SkillUnlinkArgs {
    #[arg(long)]
    pub json: bool,
}

impl Execute for SkillUnlinkArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = orbit_core::bootstrap::init::unlink_skills(&runtime.global_root())?;
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
