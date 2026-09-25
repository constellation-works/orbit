use orbit_common::OrbitError;
use orbit_types::plugin::PluginProvenance;

use super::super::callback::{
    ORBIT_PLUGIN_CALLBACK_ENV, ORBIT_PLUGIN_CALLBACK_FD_ENV, PluginCallbackSession,
    resolve_plugin_callback, stale_plugin_callback_session_count,
};

/// The host answer for a release that still honours the retired environment
/// token and process ancestry.
fn legacy_on() -> Result<bool, OrbitError> {
    Ok(true)
}

/// The default: the inherited descriptor is the only credential.
fn legacy_off() -> Result<bool, OrbitError> {
    Ok(false)
}

fn provenance(name: &str) -> PluginProvenance {
    PluginProvenance {
        name: name.into(),
        version: "1.0.0".into(),
        manifest_digest: "abc".into(),
        grants: vec!["orbit_tools".into()],
    }
}

/// Mint a session whose ceiling is the plugin's full requested list; the
/// ceiling's own behaviour has its own tests below.
fn mint(root: &std::path::Path, name: &str) -> PluginCallbackSession {
    PluginCallbackSession::mint(root, &provenance(name), &["orbit.task.list".to_string()])
        .expect("mint")
}

fn present_token(token: &str) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([
        (ORBIT_PLUGIN_CALLBACK_ENV, Some(token)),
        (ORBIT_PLUGIN_CALLBACK_FD_ENV, None),
    ])
}

fn clear_token() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([
        (ORBIT_PLUGIN_CALLBACK_ENV, None),
        (ORBIT_PLUGIN_CALLBACK_FD_ENV, None),
    ])
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

/// A file name the session directory would really use: 64 lowercase hex
/// characters, derived from a readable label so a failure names the record.
fn token_for(label: &str) -> String {
    let mut token: String = label.bytes().map(|byte| format!("{byte:02x}")).collect();
    token.truncate(64);
    while token.len() < 64 {
        token.push('0');
    }
    token
}

fn write_record(
    root: &std::path::Path,
    name: &str,
    pid: u32,
    starttime: u64,
) -> std::path::PathBuf {
    let dir = root.join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    let token = token_for(name);
    let path = dir.join(&token);
    std::fs::write(
        &path,
        format!(
            r#"{{"schema_version":3,"plugin":"stale","version":"1.0.0","manifest_digest":"abc","effective_tools":[],"token":"{token}","pid":{pid},"starttime":{starttime}}}"#
        ),
    )
    .expect("write callback record");
    path
}

mod credential;
mod legacy;
