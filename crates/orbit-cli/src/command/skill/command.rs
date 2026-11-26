use clap::{Args, Subcommand};
use orbit_core::Core;

use crate::command::{CommandOut, ExecuteCore};

use super::doctor::SkillDoctorArgs;
use super::link::SkillLinkArgs;
use super::list::SkillListArgs;
use super::show::SkillShowArgs;
use super::unlink::SkillUnlinkArgs;

#[derive(Args)]
#[command(about = "Manage agent skill definitions")]
pub struct SkillCommand {
    #[command(subcommand)]
    pub command: SkillSubcommand,
}

impl ExecuteCore for SkillCommand {
    fn execute(self, core: &Core) -> CommandOut {
        self.command.execute(core)
    }
}

#[derive(Subcommand)]
pub enum SkillSubcommand {
    /// List all available skills
    List(SkillListArgs),
    /// Show detailed information about a skill
    Show(SkillShowArgs),
    /// Validate skill health and configuration
    Doctor(SkillDoctorArgs),
    /// Re-create skill symlinks in ~/.agents/skills/ and ~/.claude/skills/
    Link(SkillLinkArgs),
    /// Remove skill symlinks from ~/.agents/skills/ and ~/.claude/skills/
    Unlink(SkillUnlinkArgs),
}

impl ExecuteCore for SkillSubcommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self {
            SkillSubcommand::List(args) => args.execute(core),
            SkillSubcommand::Show(args) => args.execute(core),
            SkillSubcommand::Doctor(args) => args.execute(core),
            SkillSubcommand::Link(args) => args.execute(core),
            SkillSubcommand::Unlink(args) => args.execute(core),
        }
    }
}
