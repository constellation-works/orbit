//! Focused credential-mask, alias and descriptor safety guards.

use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use super::*;
use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;

#[cfg(target_os = "linux")]
fn profile(modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify,
    }
}

mod credentials;
mod descriptor;
mod mask;
