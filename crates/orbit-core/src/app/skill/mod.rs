//! The skill app: file-skill listing, inspection, doctor diagnostics and
//! client discovery-link management (`orbit skill`).

mod catalog;

mod doctor;

mod materialize;

use orbit_common::OrbitError;

use super::exec::{self, App, Operation};
use crate::Core;
pub use crate::bootstrap::init::{LinkResult, UnlinkResult};
pub use crate::skill_catalog::LoadedSkill;
pub(crate) use catalog::{DEFAULT_SKILL_FILES, default_skill_ids};
pub use doctor::{SkillDoctorResult, SkillDoctorStatus};
pub(crate) use doctor::{dangling_client_skill_links, skill_link_remediation, skill_link_roots};
pub(crate) use materialize::{
    inject_skill_template_tokens, is_default_skill_tree_for_root, seed_default_skills,
};

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
            Command::List => Outcome::Listed(doctor::list_file_skills(runtime)?),
            Command::Show(name) => {
                Outcome::Shown(Box::new(doctor::show_file_skill(runtime, &name)?))
            }
            Command::Doctor { name } => {
                let rows = doctor::doctor_file_skills(runtime)?;
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
