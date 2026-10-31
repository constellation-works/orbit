//! Embed source identity while building, never by consulting a run's workspace.
#![allow(clippy::print_stdout)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// This build script cannot depend on the crate it is compiling. Keep the
// metadata probe bounded with std alone; failure embeds unknown provenance.
fn git_output(args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut output = String::new();
                child
                    .stdout
                    .take()?
                    .take(65536)
                    .read_to_string(&mut output)
                    .ok()?;
                return Some(output.trim().to_string());
            }
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn main() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_checkout = match (
        git_output(&["rev-parse", "--show-toplevel"])
            .and_then(|path| std::fs::canonicalize(path).ok()),
        std::fs::canonicalize(workspace),
    ) {
        (Some(checkout), Ok(workspace)) => checkout == workspace,
        _ => false,
    };
    // Watch both worktree HEAD and the shared branch ref (including packed
    // refs), so a rebuild after checkout or commit cannot reuse stale identity.
    for name in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git_output(&["rev-parse", "--path-format=absolute", "--git-path", name])
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git_output(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git_output(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &reference,
        ])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    // Track source edits too: a dirty build must never prove fix ancestry.
    for path in [
        "../",
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../../scripts",
        "../../.cargo",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let commit = if source_checkout {
        git_output(&["rev-parse", "--verify", "HEAD"]).unwrap_or_default()
    } else {
        String::new()
    };
    let clean = !commit.is_empty()
        && git_output(&["status", "--porcelain", "--untracked-files=normal"])
            .is_some_and(|status| status.is_empty());
    println!("cargo:rustc-env=ORBIT_BUILD_SOURCE_COMMIT={commit}");
    println!("cargo:rustc-env=ORBIT_BUILD_DIRTY={}", !clean);
}
