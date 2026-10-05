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

#[test]
fn config_writes_are_refused_in_flag_and_subcommand_form() {
    refuse(&["config", "remote.origin.url", "/tmp/repo"]);
    refuse(&["config", "--global", "user.email", "agent@example.com"]);
    refuse(&["config", "--unset", "remote.origin.url"]);
    refuse(&["config", "set", "remote.origin.url", "/tmp/repo"]);
    refuse(&["config", "--file", ".git/config", "remote.origin.url", "x"]);
}
