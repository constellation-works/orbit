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

/// Keep bounded, text-output setup callers off the production argv policy.
/// Some integration binaries include this source only for the command builder.
#[allow(dead_code)]
pub(crate) fn run(
    repo: &std::path::Path,
    args: &[&str],
) -> Result<orbit_common::fs::git::GitCommandOutput, orbit_common::OrbitError> {
    let mut git = command();
    git.args(args)
        .current_dir(repo)
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .env("GIT_TERMINAL_PROMPT", "0");
    // Preserve these callers' stronger configuration isolation as well.
    for (name, _) in std::env::vars_os() {
        if name == "GIT_CONFIG" || name.to_string_lossy().starts_with("GIT_CONFIG_") {
            git.env_remove(name);
        }
    }
    let output = orbit_common::process::run_bounded_capped(
        &mut git,
        orbit_common::fs::git::GIT_LOCAL_TIMEOUT,
        orbit_common::fs::git::GIT_OUTPUT_LIMIT,
    )?;
    Ok(orbit_common::fs::git::GitCommandOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}
