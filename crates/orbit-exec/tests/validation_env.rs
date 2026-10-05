#![allow(missing_docs)]
#![cfg(unix)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The environment owner-side required validation runs in [ORB-13987]: PATH
//! and toolchain locators come from the user's login shell rather than from
//! whatever launched the worker, an operator can prepend or replace PATH and
//! disable the probe, and every resolution says which source decided PATH.
//!
//! Each case probes a real process: a substitute login shell script that
//! behaves like a profile-reading shell (prints a banner, exports PATH and a
//! toolchain locator plus an unrelated secret), or a real `bash`/`zsh` with a
//! disposable `HOME` whose profile extends PATH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_exec::{
    EnvironmentMode, ExecRequest, LoginShell, NoSandbox, StdinMode, ValidationEnvPolicy,
    ValidationEnvSource, ValidationEnvironment, ValidationPathMode, run_process,
};
use tempfile::TempDir;

/// What `env -i PATH=/usr/bin:/bin orbit run auto …` hands the worker.
const MINIMAL_PATH: &str = "/usr/bin:/bin";

struct Host {
    _dir: TempDir,
    home: PathBuf,
    tools: PathBuf,
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tools = home.join(".toolchain/bin");
        fs::create_dir_all(&tools).unwrap();
        executable(
            &tools.join("orbit-fake-tool-13987"),
            "#!/bin/sh\necho toolchain-ok\n",
        );
        Self {
            _dir: dir,
            home,
            tools,
        }
    }

    /// The minimal environment a non-interactive launcher provides.
    fn launcher_env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".to_string(), self.home.display().to_string()),
            ("PATH".to_string(), MINIMAL_PATH.to_string()),
            ("USER".to_string(), "orbit-test".to_string()),
        ]
    }

    /// A login shell whose profile prints a banner, extends PATH, exports a
    /// toolchain locator and an unrelated secret, then runs `-c`.
    fn fake_login_shell(&self, name: &str, body_before_exec: &str) -> PathBuf {
        let shell = self.home.join(name);
        executable(
            &shell,
            &format!(
                "#!/bin/sh\n\
                 [ \"$1\" = -l ] && [ \"$2\" = -c ] || exit 64\n\
                 echo 'Welcome to the fake login shell'\n\
                 {body_before_exec}\n\
                 PATH=\"{tools}:$PATH\"; export PATH\n\
                 CARGO_HOME=\"$HOME/.cargo\"; export CARGO_HOME\n\
                 ORBIT_TEST_LOGIN_SECRET=leaked; export ORBIT_TEST_LOGIN_SECRET\n\
                 eval \"$3\"\n",
                tools = self.tools.display()
            ),
        );
        shell
    }

    fn login_path(&self) -> String {
        format!("{}:{MINIMAL_PATH}", self.tools.display())
    }
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn var<'a>(env: &'a ValidationEnvironment, name: &str) -> Option<&'a str> {
    env.env
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn shell(program: &Path) -> LoginShell {
    LoginShell::new(program, Duration::from_secs(10))
}

/// Run `command` the way required validation does: `/bin/sh -c` over the
/// resolved environment and nothing else.
fn run_with(env: &ValidationEnvironment, command: &str) -> (bool, String) {
    let outcome = run_process(
        &ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            current_dir: None,
            timeout_ms: Some(10_000),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(env.env.clone()),
            debug: false,
        },
        &NoSandbox,
    )
    .unwrap();
    (
        outcome.success,
        format!("{}{}", outcome.stdout, outcome.stderr),
    )
}

/// A worker launched with only `/usr/bin:/bin` still runs required commands
/// with the login shell's toolchain PATH; only the toolchain variables cross,
/// never the rest of the login environment.
#[test]
fn a_minimal_launcher_path_gets_the_login_shell_toolchain() {
    let host = Host::new();
    let login = host.fake_login_shell("fake-login-sh", "");

    let resolved = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy::default(),
        &shell(&login),
    );

    assert_eq!(resolved.source, ValidationEnvSource::LoginShell);
    assert_eq!(resolved.path(), Some(host.login_path().as_str()));
    assert_eq!(resolved.login_shell.as_deref(), Some(login.as_path()));
    assert_eq!(
        var(&resolved, "CARGO_HOME"),
        Some(format!("{}/.cargo", host.home.display()).as_str())
    );
    assert_eq!(
        var(&resolved, "ORBIT_TEST_LOGIN_SECRET"),
        None,
        "only toolchain variables cross from the login environment"
    );
    assert_eq!(var(&resolved, "USER"), Some("orbit-test"));
    assert_eq!(resolved.preflight_warning(), None);

    let (passed, output) = run_with(&resolved, "orbit-fake-tool-13987");
    assert!(passed, "the login-shell tool runs: {output}");
    assert_eq!(output.trim(), "toolchain-ok");

    let launcher = ValidationEnvironment::launcher(host.launcher_env());
    let (passed, output) = run_with(&launcher, "orbit-fake-tool-13987");
    assert!(!passed, "the launcher PATH alone cannot find it: {output}");
}

/// The probe quotes its printer for real login shells, which read their
/// profile from the user's home.
#[test]
fn real_login_shells_report_the_profile_path() {
    let candidates = [
        ("/bin/bash", ".bash_profile"),
        ("/bin/zsh", ".zprofile"),
        ("/usr/bin/zsh", ".zprofile"),
    ];
    let mut probed = 0;
    for (program, profile) in candidates {
        if !Path::new(program).is_file() {
            continue;
        }
        let host = Host::new();
        fs::write(
            host.home.join(profile),
            format!(
                "echo 'profile banner'\nexport PATH=\"{}:$PATH\"\n",
                host.tools.display()
            ),
        )
        .unwrap();
        let resolved = ValidationEnvironment::resolve(
            host.launcher_env(),
            &ValidationEnvPolicy::default(),
            &shell(Path::new(program)),
        );
        assert_eq!(
            resolved.source,
            ValidationEnvSource::LoginShell,
            "{program}: {:?}",
            resolved.login_shell_error
        );
        let path = resolved.path().unwrap();
        assert!(
            path.split(':')
                .next()
                .is_some_and(|first| first == host.tools.display().to_string()),
            "{program} resolved PATH={path}"
        );
        let (passed, output) = run_with(&resolved, "orbit-fake-tool-13987");
        assert!(passed, "{program}: {output}");
        probed += 1;
    }
    assert!(probed > 0, "no bash or zsh on this host to probe");
}

/// `workflow.validation_env.path` prepends to (or replaces) the resolved
/// PATH, expands `~/` against the environment's HOME, and is recorded as the
/// `config` source. Replacing drops login-shell entries, which the preflight
/// names.
#[test]
fn configured_path_prepends_or_replaces_and_is_the_recorded_source() {
    let host = Host::new();
    let login = host.fake_login_shell("fake-login-sh", "");
    let configured = vec!["~/.extra/bin".to_string(), "/opt/orbit/bin".to_string()];
    let expanded = format!("{}/.extra/bin:/opt/orbit/bin", host.home.display());

    let prepended = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy {
            path: configured.clone(),
            ..ValidationEnvPolicy::default()
        },
        &shell(&login),
    );
    assert_eq!(prepended.source, ValidationEnvSource::Config);
    assert_eq!(
        prepended.path(),
        Some(format!("{expanded}:{}", host.login_path()).as_str())
    );
    assert_eq!(prepended.preflight_warning(), None);

    let replaced = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy {
            path: configured,
            path_mode: ValidationPathMode::Replace,
            ..ValidationEnvPolicy::default()
        },
        &shell(&login),
    );
    assert_eq!(replaced.source, ValidationEnvSource::Config);
    assert_eq!(replaced.path(), Some(expanded.as_str()));
    let tools = host.tools.display().to_string();
    assert_eq!(
        replaced.missing_login_path_entries(),
        vec![tools.clone(), "/usr/bin".to_string(), "/bin".to_string()]
    );
    let warning = replaced
        .preflight_warning()
        .expect("replace drops login entries");
    assert!(
        warning.contains("lacks login-shell PATH entries") && warning.contains(&tools),
        "{warning}"
    );
}

/// A login shell that fails or hangs leaves the launcher PATH in place as
/// `launcher_fallback`, with the reason kept for the preflight warning.
#[test]
fn a_failing_or_hanging_login_shell_falls_back_to_the_launcher() {
    let host = Host::new();
    let broken = host.fake_login_shell("broken-login-sh", "echo 'profile error' >&2; exit 3");

    let failed = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy::default(),
        &shell(&broken),
    );
    assert_eq!(failed.source, ValidationEnvSource::LauncherFallback);
    assert_eq!(failed.path(), Some(MINIMAL_PATH));
    let error = failed.login_shell_error.clone().unwrap();
    assert!(
        error.contains("exited with status 3") && error.contains("profile error"),
        "{error}"
    );
    let warning = failed.preflight_warning().unwrap();
    assert!(
        warning.contains("could not resolve the login-shell environment")
            && warning.contains(&format!("PATH={MINIMAL_PATH}"))
            && warning.contains("launcher_fallback"),
        "{warning}"
    );

    let hanging = host.fake_login_shell("hanging-login-sh", "sleep 30");
    let timed_out = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy::default(),
        &LoginShell::new(&hanging, Duration::from_millis(300)),
    );
    assert_eq!(timed_out.source, ValidationEnvSource::LauncherFallback);
    assert!(
        timed_out
            .login_shell_error
            .as_deref()
            .is_some_and(|error| error.contains("did not finish")),
        "{:?}",
        timed_out.login_shell_error
    );
}

/// With resolution disabled the login shell is never started; the launcher
/// PATH is used and the preflight says so.
#[test]
fn disabled_resolution_never_starts_the_login_shell() {
    let host = Host::new();
    let marker = host.home.join("login-shell-ran");
    let login = host.fake_login_shell(
        "recording-login-sh",
        &format!("touch '{}'", marker.display()),
    );

    let resolved = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy {
            login_shell: false,
            ..ValidationEnvPolicy::default()
        },
        &shell(&login),
    );

    assert!(!marker.exists(), "the login shell must not run");
    assert_eq!(resolved.source, ValidationEnvSource::LauncherFallback);
    assert_eq!(resolved.path(), Some(MINIMAL_PATH));
    let warning = resolved.preflight_warning().unwrap();
    assert!(
        warning.contains("login-shell resolution is disabled"),
        "{warning}"
    );
}

/// A resolution is reused within the cache window rather than re-running
/// every profile per command.
#[test]
fn a_resolution_is_reused_within_the_cache_window() {
    let host = Host::new();
    let count = host.home.join("probes");
    let login = host.fake_login_shell(
        "counting-login-sh",
        &format!("echo probe >> '{}'", count.display()),
    );

    for _ in 0..3 {
        let resolved = ValidationEnvironment::resolve(
            host.launcher_env(),
            &ValidationEnvPolicy::default(),
            &shell(&login),
        );
        assert_eq!(resolved.source, ValidationEnvSource::LoginShell);
    }

    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);
}
