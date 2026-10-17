//! Git commands for this crate's disposable test repositories.

use std::process::Command;

/// Build a command without inherited repository or Orbit authority.
/// Set deliberate fixture environment and arguments after this scrub.
pub(crate) fn command() -> Command {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
}
