//! The skill app: file-skill listing, inspection, doctor diagnostics and
//! client discovery-link management (`orbit skill`).

use orbit_common::OrbitError;

use super::exec::{self, App, Operation};
use crate::Core;
pub use crate::application::skill::{SkillDoctorResult, SkillDoctorStatus};
pub use crate::bootstrap::init::{LinkResult, UnlinkResult};
pub use crate::skill_catalog::LoadedSkill;

/// One skill command.
pub enum Command {
    /// List installed file skills.
    List,
    /// Show one installed file skill.
    Show(String),
    /// Run skill health and discovery-link diagnostics, optionally filtered to
    /// one skill name.
    Doctor {
        /// Optional exact skill name to filter diagnostics to.
        name: Option<String>,
    },
    /// Re-create core skill symlinks beside the host global root.
    Link,
    /// Remove core skill symlinks beside the host global root.
    Unlink,
}

/// What a skill command produced, before the caller renders it.
pub enum Outcome {
    /// Installed file skills.
    Listed(Vec<LoadedSkill>),
    /// One installed file skill.
    Shown(Box<LoadedSkill>),
    /// Skill doctor diagnostic rows.
    Diagnosed(Vec<SkillDoctorResult>),
    /// Skill symlink creation report.
    Linked(LinkResult),
    /// Skill symlink removal report.
    Unlinked(UnlinkResult),
}

/// Why a skill command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The flow failed.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct SkillApp;

impl App for SkillApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        let (subcommand, target) = match command {
            Command::List => ("list", None),
            Command::Show(name) => ("show", Some(name.clone())),
            Command::Doctor { .. } => ("doctor", None),
            Command::Link => ("link", None),
            Command::Unlink => ("unlink", None),
        };
        Operation::command("skill", subcommand).target(Some("skill"), target)
    }

    fn run(core: &Core, command: Self::Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::List => Outcome::Listed(runtime.list_file_skills()?),
            Command::Show(name) => Outcome::Shown(Box::new(runtime.show_file_skill(&name)?)),
            Command::Doctor { name } => {
                let rows = runtime.doctor_file_skills()?;
                if let Some(name) = name {
                    let selected = rows
                        .into_iter()
                        .filter(|row| row.skill_name == name)
                        .collect::<Vec<_>>();
                    if selected.is_empty() {
                        return Err(OrbitError::InvalidInput(format!(
                            "no skill diagnostic named '{name}'"
                        ))
                        .into());
                    }
                    return Ok(Outcome::Diagnosed(selected));
                }
                Outcome::Diagnosed(rows)
            }
            Command::Link => {
                Outcome::Linked(crate::bootstrap::init::link_skills(&runtime.global_root())?)
            }
            Command::Unlink => Outcome::Unlinked(crate::bootstrap::init::unlink_skills(
                &runtime.global_root(),
            )?),
        })
    }
}

/// Authorize, audit and run one skill command.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<SkillApp>(core, command)
}
