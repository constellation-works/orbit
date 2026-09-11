use orbit_common::OrbitError;

use super::enforce_no_persistent_git_config;

fn refuse(args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    match enforce_no_persistent_git_config("proc.spawn", "git", &args) {
        Err(OrbitError::PolicyDenied(message)) => message,
        Err(other) => panic!("expected a policy denial, got: {other}"),
        Ok(()) => panic!("`git {}` must be refused", args.join(" ")),
    }
}

fn allow(args: &[&str]) {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    enforce_no_persistent_git_config("proc.spawn", "git", &args)
        .unwrap_or_else(|error| panic!("`git {}` must be allowed: {error}", args.join(" ")));
}

/// The mutation observed in ORB-12103: a recovery agent adding an `origin` that
/// points at the primary checkout, which a linked worktree shares.
#[test]
fn adding_a_remote_is_refused_even_with_a_directory_override() {
    let message = refuse(&[
        "-C",
        "/tmp/worktree",
        "remote",
        "add",
        "origin",
        "/tmp/repo",
    ]);
    assert!(
        message.contains("`git remote`") && message.contains(".git/config"),
        "denial must name the boundary it enforces, got: {message}"
    );
}

#[test]
fn every_remote_mutation_subcommand_is_refused() {
    for subcommand in ["add", "rename", "remove", "rm", "set-url", "set-head"] {
        refuse(&["remote", subcommand, "origin", "value"]);
    }
}

#[test]
fn config_writes_are_refused_in_flag_and_subcommand_form() {
    refuse(&["config", "remote.origin.url", "/tmp/repo"]);
    refuse(&["config", "--global", "user.email", "agent@example.com"]);
    refuse(&["config", "--unset", "remote.origin.url"]);
    refuse(&["config", "set", "remote.origin.url", "/tmp/repo"]);
    refuse(&["config", "--file", ".git/config", "remote.origin.url", "x"]);
}

#[test]
fn read_only_git_inspection_still_runs() {
    allow(&["remote", "-v"]);
    allow(&["remote", "show", "origin"]);
    allow(&["remote", "get-url", "origin"]);
    allow(&["config", "--get", "remote.origin.url"]);
    allow(&["config", "--list"]);
    allow(&["config", "get", "remote.origin.url"]);
    allow(&["-C", "/tmp/worktree", "status", "--porcelain"]);
    allow(&["log", "--oneline", "-n", "5"]);
    allow(&[]);
}

/// `-c` configures one invocation and leaves nothing behind, so it stays
/// allowed — but it must not hide the subcommand from the scan.
#[test]
fn per_invocation_overrides_do_not_mask_the_subcommand() {
    allow(&["-c", "core.hooksPath=/dev/null", "commit", "-m", "message"]);
    refuse(&[
        "-c",
        "core.hooksPath=/dev/null",
        "remote",
        "add",
        "origin",
        "/tmp/repo",
    ]);
}

#[test]
fn other_programs_are_not_inspected() {
    let args: Vec<String> = ["remote", "add", "origin", "/tmp/repo"]
        .iter()
        .map(ToString::to_string)
        .collect();
    enforce_no_persistent_git_config("proc.spawn", "/usr/bin/rg", &args).expect("non-git program");
}

#[test]
fn git_is_recognized_through_its_path_and_executable_suffix() {
    let args: Vec<String> = ["remote", "add", "origin", "/tmp/repo"]
        .iter()
        .map(ToString::to_string)
        .collect();
    for program in ["/usr/bin/git", "git.exe"] {
        assert!(
            matches!(
                enforce_no_persistent_git_config("proc.spawn", program, &args),
                Err(OrbitError::PolicyDenied(_))
            ),
            "`{program}` must be recognized as git"
        );
    }
}
