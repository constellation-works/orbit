//! Claimed candidate path and tree-mode validation at the exported handoff boundary.
//! ORB-14082: a tracked file's type change must not bypass symlink/gitlink refusal.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use orbit_engine::validate_claim_new_paths;
use tempfile::tempdir;

/// Protected names must be refused even when Git's candidate tree was made
/// on a host that distinguishes case and the admission selector covers them.
#[test]
fn claimed_candidate_protected_names_ignore_ascii_case_on_every_host() {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    git(repo, &["init", "-q"]);
    fs::write(repo.join("doc.md"), "base\n").unwrap();
    git(repo, &["add", "doc.md"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    let blob = git(repo, &["hash-object", "-w", "doc.md"]);

    #[cfg(target_os = "linux")]
    {
        fs::create_dir(repo.join(".orbit")).unwrap();
        fs::write(repo.join(".orbit/owner"), "owner state\n").unwrap();
        fs::create_dir(repo.join(".Orbit")).expect("the Linux fixture must distinguish case");
        fs::write(repo.join(".Orbit/candidate"), "candidate\n").unwrap();
        assert!(
            !repo.join(".orbit/candidate").exists() && !repo.join(".Orbit/owner").exists(),
            "the claimed-delivery regression must exercise a case-sensitive filesystem"
        );
    }

    for (path, accepted) in [
        (".Orbit/x", false),
        (".ORBIT/state/y", false),
        ("nested/.oRbIt/config.toml", false),
        (".GIT/config", false),
        ("nested/.GiT/config", false),
        (".ENV", false),
        (".Env.local", false),
        (".envrc", false),
        (".ENVRC", false),
        ("config/.EnvRc", false),
        ("config/Prod.ENV", false),
        ("config/Prod.eNv.local", false),
        ("config/.ENV.local/settings", false),
        ("docs/orbital.md", true),
        ("docs/ORBITAL.md", true),
        ("src/environment.rs", true),
        ("src/ENVIRONMENT.rs", true),
        ("config/.ENVIRONMENT", true),
        ("docs/.ORBITAL.md", true),
        (".GITHUB/workflows/check.yml", true),
    ] {
        // Build immutable Git trees directly: Git and the local filesystem
        // may themselves refuse protected spellings, but the owner must
        // validate an untrusted published tree without checking it out.
        let mut object = blob.clone();
        let mut mode = "100644 blob";
        let parts = path.split('/').collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate().rev() {
            let mut entries = format!("{mode} {object}\t{part}\n");
            if index == 0 {
                entries.push_str(&format!("100644 blob {blob}\tdoc.md\n"));
            }
            object = git_with_input(repo, &["mktree"], entries.as_bytes());
            mode = "040000 tree";
        }
        let candidate = git(
            repo,
            &["commit-tree", &object, "-p", &base, "-m", "candidate"],
        );
        for selector in [format!("file:{path}"), "file:other.md".into()] {
            let covered = selector == format!("file:{path}");
            let validated = validate_claim_new_paths(repo, &[selector], &base, &candidate);
            if accepted {
                let (added, widening) = validated.unwrap_or_else(|error| {
                    panic!("ordinary path {path} must be accepted: {error}")
                });
                assert_eq!(added, [path], "ordinary paths remain deliverable");
                assert_eq!(
                    widening,
                    if covered {
                        Vec::new()
                    } else {
                        vec![path.to_string()]
                    },
                    "ordinary additions widen only their own path"
                );
            } else {
                let error = validated.expect_err(
                    "protected metadata and environment paths must fail closed regardless of case",
                );
                assert!(
                    error.to_string().contains(path),
                    "refusal must name the protected candidate path {path}"
                );
            }
        }
    }
    assert_eq!(git(repo, &["rev-parse", "HEAD"]), base);
    assert_eq!(fs::read_to_string(repo.join("doc.md")).unwrap(), "base\n");
}

#[test]
fn claimed_candidate_type_changes_refuse_non_regular_files_without_widening_existing_paths() {
    for (base_mode, candidate_mode, accepted) in [
        ("100644", "120000", false),
        ("100644", "160000", false),
        ("100644", "100755", true),
        ("120000", "100644", true),
    ] {
        let temp = tempdir().unwrap();
        let repo = temp.path();
        git(repo, &["init", "-q"]);
        fs::write(repo.join("doc.md"), "base\n").unwrap();
        let base_blob = git(repo, &["hash-object", "-w", "doc.md"]);
        git(
            repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                base_mode,
                &base_blob,
                "doc.md",
            ],
        );
        git(repo, &["commit", "-q", "-m", "base"]);
        let base = git(repo, &["rev-parse", "HEAD"]);

        // Construct the candidate's actual Git tree, including a gitlink
        // without needing to clone a submodule or follow the symlink target.
        fs::write(repo.join("doc.md"), "../../../etc/passwd\n").unwrap();
        let candidate_object = if candidate_mode == "160000" {
            base.clone()
        } else {
            git(repo, &["hash-object", "-w", "doc.md"])
        };
        git(
            repo,
            &[
                "update-index",
                "--cacheinfo",
                candidate_mode,
                &candidate_object,
                "doc.md",
            ],
        );
        git(repo, &["commit", "-q", "-m", "candidate"]);
        let candidate = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["reset", "--hard", &base]);

        // Validate while the checkout holds the base, as on the accepting
        // owner. Both an exact selector and an uncovered path must be safe.
        for selector in ["file:doc.md", "file:other.md"] {
            let validated = validate_claim_new_paths(repo, &[selector.into()], &base, &candidate);
            if accepted {
                assert_eq!(
                    validated.unwrap(),
                    (Vec::new(), Vec::new()),
                    "existing regular files require no added paths or footprint widening"
                );
            } else {
                let error = validated.expect_err(
                    "ORB-14082: tracked files becoming symlinks or gitlinks must fail closed",
                );
                assert!(
                    error.to_string().contains("doc.md"),
                    "refusal must identify the unsafe {base_mode} -> {candidate_mode} path"
                );
            }
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    git_with_input(repo, args, &[])
}

fn git_with_input(repo: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let mut child = command
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@orbit.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
