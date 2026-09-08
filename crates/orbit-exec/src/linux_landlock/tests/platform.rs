//! Platform availability. Enforcement is Linux-only, and a host that cannot
//! enforce must say so rather than run the child unconfined.

use crate::linux_landlock::{MINIMUM_LANDLOCK_ABI, landlock_unavailable_message, probe_landlock};

#[cfg(not(target_os = "linux"))]
use {
    crate::linux_landlock::spawn_under_linux_landlock,
    crate::runner::{EnvironmentMode, ExecRequest, StdinMode},
    orbit_common::OrbitError,
    orbit_types::policy::ResolvedFsProfile,
    std::path::Path,
};

#[cfg(not(target_os = "linux"))]
fn request() -> ExecRequest {
    ExecRequest {
        program: "/bin/echo".to_string(),
        args: vec!["must-not-run".to_string()],
        current_dir: None,
        timeout_ms: Some(1_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(Vec::new()),
        debug: false,
    }
}

#[cfg(not(target_os = "linux"))]
fn profile() -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["**".to_string()],
        modify: vec!["**".to_string()],
    }
}

#[test]
fn an_unenforceable_host_explains_what_it_needs() {
    let probe = probe_landlock();
    let message = landlock_unavailable_message(&probe);

    assert!(
        message.contains("Landlock") && message.contains(&MINIMUM_LANDLOCK_ABI.to_string()),
        "the capability error must name the requirement: {message}"
    );
}

/// The ABI floor exists because `REFER` — the right that stops a denied file
/// from being relocated into a readable directory — arrived in ABI 2.
#[cfg(target_os = "linux")]
#[test]
fn availability_requires_the_abi_that_carries_the_relocation_right() {
    let probe = probe_landlock();

    assert_eq!(probe.available, probe.abi >= MINIMUM_LANDLOCK_ABI);
}

/// Off Linux there is no ruleset to apply, so the spawn is refused. Falling
/// back to an unconfined child would leave the alias bypass open on that
/// platform while every other layer claimed it was closed.
#[cfg(not(target_os = "linux"))]
#[test]
fn a_platform_without_landlock_refuses_to_spawn_at_all() {
    let error = spawn_under_linux_landlock(&request(), Path::new("."), &profile())
        .err()
        .expect("an unsupported platform must not spawn");

    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error:?}");
}
