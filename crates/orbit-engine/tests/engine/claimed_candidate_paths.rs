//! Claimed candidate tree-mode validation at the exported handoff boundary.
//! ORB-14082: a tracked file's type change must not bypass symlink/gitlink refusal.

use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_engine::validate_claim_new_paths;
use tempfile::tempdir;

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
    let output = Command::new("git")
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
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
