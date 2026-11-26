pub(super) use std::fs;
#[cfg(target_os = "linux")]
pub(super) use std::os::fd::AsRawFd;
pub(super) use std::path::PathBuf;

pub(super) use crate::linux_sandbox::*;
pub(super) use orbit_common::OrbitError;
#[cfg(target_os = "linux")]
pub(super) use orbit_types::policy::ResolvedFsProfile;

#[cfg(target_os = "linux")]
pub(super) fn profile(modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify,
    }
}
