#![allow(missing_docs)]

use tempfile::tempdir;

use super::super::launcher::{
    SUPPORTED_SYSTEM_BIN_DIRS, locate_provider_launcher, orbit_tool_env_with,
    resolve_provider_launcher_with, resolve_provider_launcher_with_extra_dirs,
};
use super::test_support::write_executable;

#[test]
fn provider_launcher_resolution_falls_back_to_temp_home_with_scrubbed_path() {
    let temp = tempdir().expect("tempdir");
    let fake_path = temp.path().join("system-bin");
    let home = temp.path().join("home");
    let provider_bin = home.join(".local/bin");
    std::fs::create_dir_all(&fake_path).expect("create fake PATH");
    std::fs::create_dir_all(&provider_bin).expect("create provider bin");
    let launcher = provider_bin.join("claude");
    write_executable(&launcher, "#!/bin/sh\nexit 0\n");

    let resolved = resolve_provider_launcher_with(
        "claude",
        "claude",
        Some(fake_path.as_os_str()),
        Some(&home),
        None,
    )
    .expect("HOME fallback should resolve provider");

    assert_eq!(resolved, launcher.to_string_lossy());
}

#[test]
#[cfg(unix)]
fn locate_provider_launcher_reports_only_launchable_path_like_programs() {
    let temp = tempdir().expect("tempdir");
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let launchable = bin.join("provider");
    write_executable(&launchable, "#!/bin/sh\n");
    let not_executable = bin.join("plain");
    std::fs::write(&not_executable, "not a launcher").expect("write plain file");

    assert_eq!(
        locate_provider_launcher(&launchable.to_string_lossy(), None),
        Some(launchable.clone())
    );
    assert_eq!(
        locate_provider_launcher("bin/provider", Some(temp.path())),
        Some(launchable),
        "a relative path-like program resolves against the dispatch cwd"
    );
    assert_eq!(
        locate_provider_launcher(&not_executable.to_string_lossy(), None),
        None
    );
    assert_eq!(
        locate_provider_launcher(&bin.join("missing").to_string_lossy(), None),
        None
    );
}

#[test]
fn missing_provider_launcher_error_names_provider_and_searched_locations() {
    let temp = tempdir().expect("tempdir");
    let fake_path = temp.path().join("system-bin");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&fake_path).expect("create fake PATH");
    std::fs::create_dir_all(&home).expect("create fake HOME");

    let error = resolve_provider_launcher_with(
        "claude",
        "claude",
        Some(fake_path.as_os_str()),
        Some(&home),
        None,
    )
    .expect_err("missing launcher must fail");

    assert!(error.permanent, "missing launcher must remain permanent");
    assert!(
        error.message.contains("provider `claude`"),
        "error should name the provider: {}",
        error.message
    );
    for searched in [
        fake_path.join("claude"),
        home.join(".local/bin/claude"),
        home.join(".orbit/bin/claude"),
        home.join(".cargo/bin/claude"),
        home.join("bin/claude"),
        std::path::PathBuf::from("/opt/homebrew/bin/claude"),
        std::path::PathBuf::from("/usr/local/bin/claude"),
    ] {
        assert!(
            error.message.contains(&searched.display().to_string()),
            "error should name searched location {}: {}",
            searched.display(),
            error.message
        );
    }
    assert_eq!(
        SUPPORTED_SYSTEM_BIN_DIRS,
        &["/opt/homebrew/bin", "/usr/local/bin"]
    );
}

#[test]
fn agent_tool_environment_prefers_dispatching_orbit_over_stale_path_entry() {
    let inherited = std::env::join_paths(["/home/test/.cargo/bin", "/usr/bin"])
        .expect("construct inherited PATH");
    let env = orbit_tool_env_with(
        None,
        std::path::Path::new("/home/test/.orbit/bin/orbit"),
        Some(&inherited),
        None,
    )
    .expect("pin dispatching Orbit");

    assert_eq!(
        env[0],
        (
            "ORBIT_BIN".to_string(),
            "/home/test/.orbit/bin/orbit".to_string()
        )
    );
    let pinned = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value)
        .expect("PATH override");
    assert_eq!(
        std::env::split_paths(std::ffi::OsStr::new(pinned)).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("/home/test/.orbit/bin"),
            std::path::PathBuf::from("/home/test/.cargo/bin"),
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/opt/homebrew/bin"),
            std::path::PathBuf::from("/usr/local/bin"),
        ]
    );
}

#[test]
fn configured_orbit_bin_wins_and_its_path_entry_is_deduplicated() {
    let inherited = std::env::join_paths(["/home/test/.cargo/bin", "/opt/orbit/bin", "/usr/bin"])
        .expect("construct inherited PATH");
    let env = orbit_tool_env_with(
        Some(std::ffi::OsStr::new("/opt/orbit/bin/orbit")),
        std::path::Path::new("/home/test/.orbit/bin/orbit"),
        Some(&inherited),
        None,
    )
    .expect("pin configured Orbit");

    assert_eq!(
        env[0],
        ("ORBIT_BIN".to_string(), "/opt/orbit/bin/orbit".to_string())
    );
    let pinned = &env[1].1;
    assert_eq!(
        std::env::split_paths(std::ffi::OsStr::new(pinned)).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("/opt/orbit/bin"),
            std::path::PathBuf::from("/home/test/.cargo/bin"),
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/opt/homebrew/bin"),
            std::path::PathBuf::from("/usr/local/bin"),
        ]
    );
}

#[test]
fn agent_tool_environment_backfills_conventional_home_bin_dirs_missing_from_path() {
    let inherited = std::env::join_paths(["/usr/bin"]).expect("construct inherited PATH");
    let home = std::path::Path::new("/home/test");
    let env = orbit_tool_env_with(
        None,
        std::path::Path::new("/home/test/.orbit/bin/orbit"),
        Some(&inherited),
        Some(home),
    )
    .expect("pin dispatching Orbit");

    let pinned = env
        .iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value)
        .expect("PATH override");
    assert_eq!(
        std::env::split_paths(std::ffi::OsStr::new(pinned)).collect::<Vec<_>>(),
        vec![
            std::path::PathBuf::from("/home/test/.orbit/bin"),
            std::path::PathBuf::from("/usr/bin"),
            std::path::PathBuf::from("/home/test/.local/bin"),
            std::path::PathBuf::from("/home/test/.cargo/bin"),
            std::path::PathBuf::from("/home/test/bin"),
            std::path::PathBuf::from("/opt/homebrew/bin"),
            std::path::PathBuf::from("/usr/local/bin"),
        ]
    );
}

/// launchd's default PATH on macOS. Drain, pilot, and shipment share
/// `resolve_provider_launcher`; this is the scheduled-worker environment
/// that failed to find `/opt/homebrew/bin/codex` on the live Mac cargo
/// binary `/Users/daniel/.cargo/bin/orbit` (0.19.2). Source HEAD at
/// pickup: `bca1cddc99987506c96e0ea626902f537d53572b`. [ORB-11808]
const MACOS_LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

fn homebrew_style_prefix(root: &std::path::Path) -> std::path::PathBuf {
    root.join("opt").join("homebrew").join("bin")
}

/// Launch a just-written resolver fixture.
///
/// A raw test-only `Command` keeps `io::ErrorKind` so Linux can reuse
/// [`orbit_common::test_process::retry_executable_busy`] at this boundary.
/// Other Unix targets still launch once. Fixture env matches `spawn_bare`:
/// cleared environment plus the launchd-style `PATH`.
fn launch_resolved_provider(program: &str) -> String {
    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .env("PATH", MACOS_LAUNCHD_PATH)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let output = {
        #[cfg(target_os = "linux")]
        {
            orbit_common::test_process::retry_executable_busy(|| command.output())
                .expect("spawn resolved provider launcher")
        }
        #[cfg(not(target_os = "linux"))]
        {
            command.output().expect("spawn resolved provider launcher")
        }
    };
    assert!(
        output.status.success(),
        "resolved launcher must exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn minimal_macos_path_resolves_homebrew_style_provider_for_scheduled_and_interactive_launch() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    let launcher = prefix.join("codex");
    write_executable(&launcher, "#!/bin/sh\nprintf 'codex-fixture-0.153.4\\n'\n");

    let scheduled = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix.clone()],
    )
    .expect("scheduled drain PATH must resolve Homebrew-style launcher");
    let interactive_path = format!("{MACOS_LAUNCHD_PATH}:{}", prefix.display());
    let interactive = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(&interactive_path)),
        Some(&home),
        None,
        [prefix.clone()],
    )
    .expect("interactive PATH must resolve Homebrew-style launcher");
    let pilot = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix],
    )
    .expect("pilot PATH must resolve Homebrew-style launcher");

    assert_eq!(scheduled, launcher.to_string_lossy());
    assert_eq!(interactive, scheduled);
    assert_eq!(pilot, scheduled);

    let scheduled_out = launch_resolved_provider(&scheduled);
    let interactive_out = launch_resolved_provider(&interactive);
    assert_eq!(scheduled_out, "codex-fixture-0.153.4\n");
    assert_eq!(interactive_out, scheduled_out);
}

#[cfg(unix)]
#[test]
fn explicit_launcher_path_wins_over_homebrew_style_prefix() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    let override_dir = temp.path().join("override");
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    std::fs::create_dir_all(&override_dir).expect("create override dir");
    write_executable(&prefix.join("codex"), "#!/bin/sh\nprintf 'homebrew\\n'\n");
    let override_launcher = override_dir.join("codex");
    write_executable(&override_launcher, "#!/bin/sh\nprintf 'override\\n'\n");

    let resolved = resolve_provider_launcher_with_extra_dirs(
        "codex",
        override_launcher.to_str().expect("utf-8 override path"),
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix],
    )
    .expect("explicit path must be preserved");

    assert_eq!(resolved, override_launcher.to_string_lossy());
    assert_eq!(launch_resolved_provider(&resolved), "override\n");
}

#[cfg(unix)]
#[test]
fn path_executable_wins_over_homebrew_style_fallback() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    let path_dir = temp.path().join("path-bin");
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    std::fs::create_dir_all(&path_dir).expect("create PATH dir");
    write_executable(&prefix.join("codex"), "#!/bin/sh\nprintf 'homebrew\\n'\n");
    let path_launcher = path_dir.join("codex");
    write_executable(&path_launcher, "#!/bin/sh\nprintf 'path\\n'\n");

    let resolved = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(path_dir.as_os_str()),
        Some(&home),
        None,
        [prefix],
    )
    .expect("PATH must beat Homebrew-style fallback");

    assert_eq!(resolved, path_launcher.to_string_lossy());
    assert_eq!(launch_resolved_provider(&resolved), "path\n");
}

/// The fixture launch helper must wait out a sibling writer the same way
/// `a_fresh_test_launcher_waits_for_a_writer_to_close` does, without changing
/// the resolver assertions above.
#[cfg(target_os = "linux")]
#[test]
fn launch_resolved_provider_retries_executable_file_busy() {
    let temp = tempdir().expect("tempdir");
    let launcher = temp.path().join("codex");
    write_executable(&launcher, "#!/bin/sh\nprintf 'path\\n'\n");
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&launcher)
        .expect("hold launcher open for writing");
    let error = std::process::Command::new(&launcher)
        .spawn()
        .expect_err("Linux must reject an executable that is open for writing");
    assert_eq!(error.kind(), std::io::ErrorKind::ExecutableFileBusy);

    let release_writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(75));
        drop(writer);
    });
    let stdout = launch_resolved_provider(launcher.to_str().expect("utf-8 launcher path"));
    release_writer.join().expect("release launcher writer");

    assert_eq!(stdout, "path\n");
}

/// Only `ExecutableFileBusy` is in the retry window; other spawn errors must
/// fail on the first attempt so the fixture does not mask a real resolver
/// or permission problem.
#[cfg(target_os = "linux")]
#[test]
fn retry_executable_busy_returns_non_busy_errors_immediately() {
    use std::io::{Error, ErrorKind};
    use std::time::Instant;

    use orbit_common::test_process::retry_executable_busy;

    for kind in [
        ErrorKind::NotFound,
        ErrorKind::PermissionDenied,
        ErrorKind::Other,
    ] {
        let mut attempts = 0;
        let started = Instant::now();
        let error = retry_executable_busy(|| {
            attempts += 1;
            Err::<(), _>(Error::new(kind, "boom"))
        })
        .expect_err("non-busy errors must surface immediately");
        assert_eq!(error.kind(), kind);
        assert_eq!(attempts, 1, "{kind:?} must not be retried");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "{kind:?} must not wait for the executable-busy window"
        );
    }

    let mut remaining_busy = 2;
    let mut attempts = 0;
    retry_executable_busy(|| {
        attempts += 1;
        if remaining_busy > 0 {
            remaining_busy -= 1;
            Err(Error::new(ErrorKind::ExecutableFileBusy, "busy"))
        } else {
            Ok(())
        }
    })
    .expect("ExecutableFileBusy must retry until success");
    assert_eq!(attempts, 3);
}

#[cfg(unix)]
#[test]
fn non_executable_homebrew_style_file_is_skipped_and_absent_launcher_stays_permanent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let prefix = homebrew_style_prefix(temp.path());
    std::fs::create_dir_all(&home).expect("create fake HOME");
    std::fs::create_dir_all(&prefix).expect("create Homebrew-style prefix");
    let stub = prefix.join("codex");
    std::fs::write(&stub, "#!/bin/sh\nprintf 'not-executable\\n'\n")
        .expect("write non-executable stub");

    let error = resolve_provider_launcher_with_extra_dirs(
        "codex",
        "codex",
        Some(std::ffi::OsStr::new(MACOS_LAUNCHD_PATH)),
        Some(&home),
        None,
        [prefix.clone()],
    )
    .expect_err("non-executable stub must not resolve");

    assert!(error.permanent, "missing launcher must remain permanent");
    assert!(
        error.message.contains("provider `codex`"),
        "error should name the provider: {}",
        error.message
    );
    assert!(
        error.message.contains(&stub.display().to_string()),
        "error should name the non-executable candidate: {}",
        error.message
    );
    assert!(
        error.message.contains("was not found"),
        "error should stay a missing-launcher diagnostic: {}",
        error.message
    );
}
