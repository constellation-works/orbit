use clap::Args;
use orbit_core::Core;
use orbit_core::app::doctor::{Command, exec};

use crate::command::CommandOut;

#[derive(Args)]
pub struct FsAccessArgs {
    /// Filesystem profile name (for example `implementer`)
    pub profile: String,
    /// Path to check, matched as written against the profile's workspace-relative rules
    pub path: String,
}

/// `orbit doctor fs-access`: the active policy's read and modify verdicts for
/// one path under one fsProfile, without running anything.
pub(super) fn fs_access(core: &Core, profile: String, path: String) -> CommandOut {
    let outcome = exec(
        core,
        Command::FsAccess {
            profile: profile.clone(),
            path: path.clone(),
        },
    )?;
    Ok(crate::output::doctor::fs_access_payload(&profile, &path, outcome).into())
}
