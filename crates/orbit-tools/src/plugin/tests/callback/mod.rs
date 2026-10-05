//! Callback credential fixtures kept for the forged-descriptor guard.

#[cfg(unix)]
use super::super::callback::{
    ORBIT_PLUGIN_CALLBACK_ENV, ORBIT_PLUGIN_CALLBACK_FD_ENV, PluginCallbackSession,
};
#[cfg(unix)]
use orbit_common::OrbitError;
#[cfg(unix)]
use orbit_types::plugin::PluginProvenance;

/// The default: the inherited descriptor is the only credential.
#[cfg(unix)]
fn legacy_off() -> Result<bool, OrbitError> {
    Ok(false)
}

#[cfg(unix)]
fn provenance(name: &str) -> PluginProvenance {
    PluginProvenance {
        name: name.into(),
        version: "1.0.0".into(),
        manifest_digest: "abc".into(),
        grants: vec!["orbit_tools".into()],
    }
}

#[cfg(unix)]
fn mint(root: &std::path::Path, name: &str) -> PluginCallbackSession {
    PluginCallbackSession::mint(root, &provenance(name), &["orbit.task.list".to_string()])
        .expect("mint")
}

/// Present one open descriptor on a file the way a spawned backend holds its
/// credential, with every legacy credential cleared. The returned file must
/// stay alive for as long as the descriptor is presented.
#[cfg(unix)]
fn present_descriptor(
    path: &std::path::Path,
) -> (std::fs::File, orbit_common::test_env::ScopedEnv) {
    use std::os::fd::AsRawFd;

    let file = std::fs::File::open(path).expect("open the credential");
    let number = file.as_raw_fd().to_string();
    let env = orbit_common::test_env::scoped([
        (ORBIT_PLUGIN_CALLBACK_ENV, None),
        (ORBIT_PLUGIN_CALLBACK_FD_ENV, Some(number.as_str())),
    ]);
    (file, env)
}

mod credential;
