use std::path::Path;

use orbit_engine::RuntimeHost;
use orbit_types::workflow::ExecutorSandboxKind;

use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_executor,
};

use super::resolve::{last_compiled_file_write_allows_under, last_compiled_file_write_under};

/// A security invariant at the resolver/compiler boundary: convenience grants
/// for an implementer checkout must not reopen host-owned Git metadata, either
/// in place or by renaming a checkout holding a `.git` pointer aside.
///
/// Each profile must deny its metadata with an explicit clause. The fixture
/// sits beneath the host scratch allows, so a path left to the default deny,
/// like a sibling checkout's `.git` pointer outside the active checkout's
/// grants, is writable here and is not part of this contract.
#[test]
fn macos_implementer_worktree_denies_registered_and_linked_git_metadata() {
    let (_fixture, runtime, repo) = runtime_with_workspace_layout();
    let repo = repo.canonicalize().expect("canonical repo");
    let common = repo.join(".git");
    for directory in ["info", "hooks", "worktrees"] {
        std::fs::create_dir_all(common.join(directory)).expect("metadata directory");
    }
    for file in ["config", "info/attributes", "hooks/pre-commit"] {
        std::fs::write(common.join(file), "host metadata").expect("metadata file");
    }

    let mut shared = vec![
        common.join("config"),
        common.join("info/attributes"),
        common.join("info/new-attributes"),
        common.join("hooks/pre-commit"),
        common.join("hooks/new-hook"),
        common.join("worktrees/new-worktree/HEAD"),
    ];
    let mut checkouts = Vec::new();
    for (pool, name) in [
        ("worktrees", "orbit-jrun-git-deny"),
        ("recovery-checkouts", "orbit-jrun-git-recovery"),
    ] {
        let checkout = repo.join(".orbit/state").join(pool).join(name);
        let gitdir = common.join("worktrees").join(name);
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::create_dir_all(&gitdir).expect("per-worktree gitdir");
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .expect("worktree pointer");
        std::fs::write(gitdir.join("commondir"), "../..\n").expect("common pointer");
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/candidate\n").expect("HEAD");
        shared.push(gitdir.join("HEAD"));
        checkouts.push(checkout);
    }

    for provider in ["claude", "codex"] {
        seed_executor(
            &runtime,
            provider,
            Some(ExecutorSandboxKind::MacosSandboxExec),
        );
        for checkout in &checkouts {
            let sandbox = runtime
                .resolve_executor_sandbox(provider, Some("implementer"), Some(checkout))
                .expect("resolve implementer sandbox")
                .expect("sandbox");
            let sbpl = orbit_exec::compile_macos_sandbox_profile(&sandbox.fs_profile, provider)
                .expect("compile implementer profile");
            let mut protected = shared.clone();
            protected.push(checkout.join(".git"));
            for path in &protected {
                assert_eq!(
                    last_compiled_file_write_under(&sbpl, path, &repo),
                    Some(false),
                    "{provider} from {} must deny Git metadata {}:\n{sbpl}",
                    checkout.display(),
                    path.display()
                );
            }
            for root in [checkout, &repo] {
                assert_eq!(
                    last_compiled_file_write_under(&sbpl, root, &repo),
                    Some(false),
                    "{provider} from {} must not rename or replace {}:\n{sbpl}",
                    checkout.display(),
                    root.display()
                );
            }
            let source = checkout.join("source.rs");
            assert!(
                last_compiled_file_write_allows_under(&sbpl, &source, &repo),
                "the implementer must retain source writes: {sbpl}"
            );
            if orbit_exec::macos_sandbox_test_guard(
                "macos_implementer_worktree_denies_registered_and_linked_git_metadata",
            ) {
                assert_native_writes(&sbpl, &source, &protected);
                assert_native_checkout_rename_denied(&sbpl, checkout, &repo);
            }
        }
    }
}

fn assert_native_writes(profile: &str, source: &Path, protected: &[std::path::PathBuf]) {
    let append = |path: &Path| {
        orbit_common::process::run_bounded_capped(
            std::process::Command::new("/usr/bin/sandbox-exec").args([
                "-p",
                profile,
                "/bin/sh",
                "-c",
                "printf probe >> \"$1\"",
                "git-deny-probe",
                path.to_str().expect("fixture path"),
            ]),
            std::time::Duration::from_secs(10),
            64 * 1024,
        )
        .expect("run sandbox-exec write probe")
    };
    assert!(
        append(source).status.success(),
        "source write positive control"
    );
    for path in protected.iter().filter(|path| path.exists()) {
        let before = std::fs::read(path).expect("metadata before");
        let output = append(path);
        assert!(
            !output.status.success()
                && String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "Git write must be denied for {}: {:?}",
            path.display(),
            output
        );
        assert_eq!(std::fs::read(path).expect("metadata after"), before);
    }
}

/// Seatbelt checks a rename against the moved entry only, so moving the
/// checkout aside would carry its `.git` pointer out of the pathname deny.
fn assert_native_checkout_rename_denied(profile: &str, checkout: &Path, repo: &Path) {
    let run = |script: &str, args: &[&Path]| {
        let mut command = std::process::Command::new("/usr/bin/sandbox-exec");
        command.args(["-p", profile, "/bin/sh", "-c", script, "git-rename-probe"]);
        command.args(args);
        orbit_common::process::run_bounded_capped(
            &mut command,
            std::time::Duration::from_secs(10),
            64 * 1024,
        )
        .expect("run sandbox-exec rename probe")
    };
    let fresh = checkout.join("fresh-dir/file");
    assert!(
        run(
            "mkdir -p \"$1\" && printf probe > \"$1/file\"",
            &[&checkout.join("fresh-dir")]
        )
        .status
        .success()
            && fresh.exists(),
        "a pinned checkout must still accept new entries beneath it"
    );
    let aside = repo.join("moved-checkout");
    for (from, to) in [
        (checkout, aside.as_path()),
        (repo, &repo.with_extension("moved")),
    ] {
        let output = run("mv \"$1\" \"$2\"", &[from, to]);
        assert!(
            !output.status.success() && !to.exists(),
            "renaming {} aside must be denied: {output:?}",
            from.display()
        );
    }
    assert!(
        checkout.join(".git").is_file(),
        "the checkout must stay in place"
    );
}
