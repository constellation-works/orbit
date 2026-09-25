//! Unit tests for `linux_sandbox` rule expansion — sibling layout under
//! linux_sandbox/tests/. The filesystem walk is platform-neutral, so these run
//! everywhere; the bwrap spawn itself is covered by the Linux-only
//! integration tests.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::OnceLock;

use super::*;
use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;

fn tree() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    for dir in ["a", "a/deep", "b", "target/debug"] {
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
    for file in [
        ".env",
        "a/.env",
        "a/deep/.env.local",
        "b/settings.env",
        "b/env.txt",
        "target/debug/build.env.bak",
    ] {
        fs::write(root.join(file), b"x").expect("write file");
    }
    temp
}

fn canonical(root: &std::path::Path, rel: &str) -> PathBuf {
    root.join(rel).canonicalize().expect("canonical")
}

fn profile(modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify,
    }
}

fn synthetic_probe(available: bool, detail: &str) -> BwrapProbeOutcome {
    BwrapProbeOutcome {
        available,
        trusted_path: "/usr/bin/bwrap".to_string(),
        detail: detail.to_string(),
    }
}

mod cache;
mod descriptor;
mod rules;
