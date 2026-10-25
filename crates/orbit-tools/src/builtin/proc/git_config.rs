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
//! recognize as a read is refused. Command-line alias and include definitions
//! are also refused: Git expands an alias before dispatch, a shell alias can run
//! arbitrary configuration writes, and an included file can define an `[alias]`
//! section. Ordinary `-c` / `--config-env` options remain available. The dashed
//! builtins (`git-config`, `git-remote`) are held to the same rule.

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
    "--attr-source",
];

/// `git config` invocations that only read, in both the flag form and the
/// subcommand form Git 2.46 introduced. The subcommand form counts only as the
/// first argument after `config`: later, `get` can be the value of an option
/// such as `--comment`, or the key of a legacy write.
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
    let Some(invocation) = git_invocation(program) else {
        return Ok(());
    };
    let command = match invocation {
        GitInvocation::Git => git_subcommand(args),
        GitInvocation::Dashed(name) => GitCommand::Subcommand(name, args),
    };
    let (matched_rule, reason) = match command {
        GitCommand::Alias => (
            "alias",
            "Command-line Git aliases may write persistent repository configuration",
        ),
        GitCommand::Include => (
            "include",
            "Command-line Git config includes may load aliases that write persistent repository \
             configuration",
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

enum GitInvocation<'a> {
    /// `git [options] <subcommand> …`
    Git,
    /// A dashed builtin such as `git-config`, which is its own subcommand.
    Dashed(&'a str),
}

fn git_invocation(program: &str) -> Option<GitInvocation<'_>> {
    let file_name = std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    let stem = file_name.strip_suffix(".exe").unwrap_or(file_name);
    if stem.eq_ignore_ascii_case("git") {
        return Some(GitInvocation::Git);
    }
    let prefix = stem
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("git-"))?;
    let name = &stem[prefix.len()..];
    // Subcommand names are matched exactly; fold the case of the two guarded
    // ones the way the program name above is.
    let name = ["config", "remote"]
        .into_iter()
        .find(|guarded| name.eq_ignore_ascii_case(guarded))
        .unwrap_or(name);
    Some(GitInvocation::Dashed(name))
}

enum GitCommand<'a> {
    Subcommand(&'a str, &'a [String]),
    Alias,
    Include,
    None,
}

/// The first positional argument, skipping Git's own leading options and
/// refusing alias and include definitions before they can disguise the
/// dispatched command.
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
        // rather than attempting to reproduce Git's expansion rules. An
        // `include` / `includeIf` path can load an `[alias]` section from a
        // file the caller wrote, so it is refused for the same reason.
        let section = config
            .and_then(|value| value.split_once('.'))
            .map(|(section, _)| section);
        if section.is_some_and(|section| section.eq_ignore_ascii_case("alias")) {
            return GitCommand::Alias;
        }
        if section.is_some_and(|section| {
            section.eq_ignore_ascii_case("include") || section.eq_ignore_ascii_case("includeif")
        }) {
            return GitCommand::Include;
        }
        if VALUE_TAKING_GIT_OPTIONS.contains(&arg.as_str()) {
            index += 1;
        }
        index += 1;
    }
    GitCommand::None
}

fn is_read_only_config(args: &[String]) -> bool {
    if args
        .first()
        .is_some_and(|arg| CONFIG_READ_SUBCOMMANDS.contains(&arg.as_str()))
    {
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
