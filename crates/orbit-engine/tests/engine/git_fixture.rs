//! Git safety shared by fixtures that publish to disposable bare repositories.

use std::fs;
use std::path::Path;
use std::process::Command;

pub(super) const FILE_ONLY_CONFIG: &str =
    "[protocol]\n\tallow = never\n[protocol \"file\"]\n\tallow = always\n";

pub(super) fn configure_child(command: &mut Command, home: &Path, sandbox: &Path) {
    // Engine VCS children clear GIT_* variables but retain HOME. Both layers
    // must enforce the policy, including calls through the real provider.
    fs::write(home.join(".gitconfig"), FILE_ONLY_CONFIG).unwrap();
    command
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_CEILING_DIRECTORIES", sandbox);
}

/// Isolate fixture environment and process-global caches under parallel libtest.
pub(super) fn isolated(module: &str, test: &str, body: impl FnOnce()) {
    const CHILD_ENV: &str = "ORBIT_GIT_FIXTURE_CHILD";
    let qualified = format!("{}::{test}", module.split_once("::").unwrap().1);
    if std::env::var(CHILD_ENV).as_deref() == Ok(&qualified) {
        body();
        return;
    }
    let sandbox = tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).unwrap();
    let home = sandbox.path().join("home");
    let tmp = sandbox.path().join("tmp");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GIT_") || name.starts_with("GH_") {
            command.env_remove(name.as_ref());
        }
    }
    configure_child(&mut command, &home, sandbox.path());
    command
        .args([&qualified, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(sandbox.path())
        .env(CHILD_ENV, &qualified)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TMPDIR", &tmp)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = orbit_common::test_env::run_child_test(&mut command, &qualified, sandbox.path());
    orbit_common::test_env::assert_child_test_passed(
        &qualified,
        output.status,
        output.stdout,
        output.stderr,
    );
}

pub(super) fn assert_local_push_remote(repo: &Path) {
    // get-url expands insteadOf/pushInsteadOf and --all includes every
    // pushurl, so checking only remote.origin.url cannot admit a second URL.
    let urls = run(repo, &["remote", "get-url", "--push", "--all", "origin"]);
    assert!(!urls.is_empty(), "fixture origin has no push URL");
    for url in urls.lines() {
        assert!(
            Path::new(url).is_absolute() && !url.starts_with("//") && !url.starts_with("\\\\"),
            "refusing fixture push to non-local origin: {url}"
        );
    }
}

pub(super) fn run(repo: &Path, args: &[&str]) -> String {
    if args.first() == Some(&"push") {
        assert_local_push_remote(repo);
    }
    let output = Command::new("git")
        .args(args)
        .env("GIT_ALLOW_PROTOCOL", "file")
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\n{}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(super) fn init(repo: &Path) {
    // Name the destination explicitly and verify ownership before any writes
    // or pushes. An absent .git must never discover the surrounding checkout.
    run(repo, &["init", "--template=", "."]);
    assert_eq!(
        Path::new(&run(repo, &["rev-parse", "--show-toplevel"]))
            .canonicalize()
            .unwrap(),
        repo.canonicalize().unwrap(),
        "fixture Git setup escaped its disposable repository"
    );
}
