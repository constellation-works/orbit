//! Persistent Git configuration is out of bounds for `proc.spawn`.
//!
//! An activity runs inside a linked worktree that shares `.git/config` with its
//! primary checkout, so a single `git remote add` performed by an agent rewrites
//! repository state that outlives the run — and can manufacture the very
//! precondition a publication step is supposed to prove. Orbit's own delivery
//! steps configure remotes through private VCS operations, never through this
//! tool, so refusing configuration writes here removes an escape hatch without
//! taking away a supported flow. Reads stay allowed: diagnosing a failure needs
//! `git remote -v` and `git config --get`.
//!
//! The rule covers the two commands that write configuration directly, `git
//! config` and `git remote`, and it fails closed: a form this module cannot
//! recognize as a read is refused. Command-line alias definitions are also
//! refused: Git expands them before dispatch, and a shell alias can run arbitrary
//! configuration writes. Ordinary non-alias `-c` / `--config-env` options remain
//! available.

use orbit_common::OrbitError;
use orbit_common::tracing;

/// Git's own options that consume the following argument, so the scan does not
/// mistake an option value for the subcommand.
const VALUE_TAKING_GIT_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--config-env",
    "--git-dir",
    "--work-tree",
    "--namespace",
];

/// `git config` invocations that only read, in both the flag form and the
/// subcommand form Git 2.46 introduced.
const CONFIG_READ_FLAGS: &[&str] = &[
    "--get",
    "--get-all",
    "--get-regexp",
    "--get-urlmatch",
    "--get-color",
    "--get-colorbool",
    "--list",
    "-l",
];
const CONFIG_READ_SUBCOMMANDS: &[&str] = &["get", "list"];

/// `git remote` invocations that only report. Every other remote subcommand —
/// `add`, `rename`, `remove`, `set-url`, `set-head`, … — writes `.git/config`.
const REMOTE_READ_SUBCOMMANDS: &[&str] = &["show", "get-url"];

/// Refuse a `git` invocation that would write persistent repository
/// configuration. Non-`git` programs pass through untouched.
pub(super) fn enforce_no_persistent_git_config(
    tool_name: &str,
    program: &str,
    args: &[String],
) -> Result<(), OrbitError> {
    if !is_git_program(program) {
        return Ok(());
    }
    let (matched_rule, reason) = match git_subcommand(args) {
        GitCommand::Alias => (
            "alias",
            "Command-line Git aliases may write persistent repository configuration",
        ),
        GitCommand::Subcommand("config", rest) if !is_read_only_config(rest) => (
            "config",
            "`git config` would write persistent repository configuration",
        ),
        GitCommand::Subcommand("remote", rest) if !is_read_only_remote(rest) => (
            "remote",
            "`git remote` would write persistent repository configuration",
        ),
        _ => return Ok(()),
    };

    tracing::warn!(
        target: "orbit.policy.deny",
        tool = tool_name,
        path = program,
        profile = "proc.persistent_git_config",
        matched_rule,
    );
    Err(OrbitError::PolicyDenied(format!(
        "{reason}, which {tool_name} \
         never permits: a worktree shares `.git/config` with its primary checkout, so the change \
         would outlive this run. Read-only queries such as `git config --get` and `git remote -v` \
         remain available."
    )))
}

fn is_git_program(program: &str) -> bool {
    let file_name = std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    let stem = file_name.strip_suffix(".exe").unwrap_or(file_name);
    stem.eq_ignore_ascii_case("git")
}

enum GitCommand<'a> {
    Subcommand(&'a str, &'a [String]),
    Alias,
    None,
}

/// The first positional argument, skipping Git's own leading options and
/// refusing alias definitions before they can disguise the dispatched command.
fn git_subcommand(args: &[String]) -> GitCommand<'_> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if !arg.starts_with('-') {
            return GitCommand::Subcommand(arg.as_str(), &args[index + 1..]);
        }
        let config = match arg.as_str() {
            "-c" | "--config-env" => args.get(index + 1).map(String::as_str),
            _ => arg
                .strip_prefix("-c")
                .or_else(|| arg.strip_prefix("--config-env=")),
        };
        // Git config section names are case-insensitive. Deny every alias
        // definition, including shell aliases and aliases of other aliases,
        // rather than attempting to reproduce Git's expansion rules.
        if config.is_some_and(|value| {
            value
                .split_once('.')
                .is_some_and(|(section, _)| section.eq_ignore_ascii_case("alias"))
        }) {
            return GitCommand::Alias;
        }
        if VALUE_TAKING_GIT_OPTIONS.contains(&arg.as_str()) {
            index += 1;
        }
        index += 1;
    }
    GitCommand::None
}

fn is_read_only_config(args: &[String]) -> bool {
    let positional = args.iter().find(|arg| !arg.starts_with('-'));
    if positional.is_some_and(|arg| CONFIG_READ_SUBCOMMANDS.contains(&arg.as_str())) {
        return true;
    }
    // The flag form reads only when it names a read mode and carries no second
    // positional to store: `git config --get a.b` reads, `git config a.b value`
    // writes. The rule fails closed, so a read that spends a second positional
    // — `--get-urlmatch`, or `--file <path>` with the path detached from its
    // option — is refused along with the writes.
    let read_flag = args
        .iter()
        .any(|arg| CONFIG_READ_FLAGS.contains(&arg.as_str()));
    read_flag && args.iter().filter(|arg| !arg.starts_with('-')).count() <= 1
}

fn is_read_only_remote(args: &[String]) -> bool {
    match args.iter().find(|arg| !arg.starts_with('-')) {
        None => true,
        Some(subcommand) => REMOTE_READ_SUBCOMMANDS.contains(&subcommand.as_str()),
    }
}

#[cfg(test)]
#[path = "tests/git_config.rs"]
mod tests;
