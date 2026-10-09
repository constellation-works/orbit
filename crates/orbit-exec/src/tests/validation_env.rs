//! The interactive probe's startup failure is the fallback reason. The probe
//! program is injected as a script that exits 42 under `-i` and behaves as a
//! login shell otherwise, so no real `bash -i` has to finish inside the
//! production timeout.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use super::{LoginShell, LoginShellMode};

/// An interactive probe that exits 42 falls back to login-only resolution, and
/// the reason carries the exit status.
#[test]
fn an_interactive_probe_exiting_42_falls_back_with_its_status_as_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let program = dir.path().join("interactive-exits-42-sh");
    fs::write(
        &program,
        "#!/bin/sh\n\
         [ \"$1\" = -i ] && exit 42\n\
         [ \"$1\" = -l ] && [ \"$2\" = -c ] || exit 64\n\
         eval \"$3\"\n",
    )
    .expect("write probe");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("chmod probe");
    let base = vec![
        ("HOME".to_string(), dir.path().display().to_string()),
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
    ];

    let resolved = LoginShell::new(&program, Duration::from_secs(10))
        .resolve(&base)
        .expect("login fallback resolves");

    assert_eq!(resolved.mode, LoginShellMode::Login);
    let reason = resolved
        .fallback_reason
        .as_deref()
        .expect("fallback reason");
    assert!(reason.contains("exited with status 42"), "{reason}");
}
