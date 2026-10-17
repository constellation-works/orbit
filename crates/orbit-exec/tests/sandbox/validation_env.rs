// Integration fixtures exercise public behavior and unwrap setup invariants.

//  The environment owner-side required validation runs in [ORB-13987]: PATH
//  and toolchain locators come from the user's login shell rather than from
//  whatever launched the worker, an operator can prepend or replace PATH and
//  disable the probe, and every resolution says which source decided PATH.
// 
//  Each case probes a real process: a substitute login shell script that
//  behaves like a profile-reading shell (prints a banner, exports PATH and a
//  toolchain locator plus an unrelated secret), or a real `bash`/`zsh` with a
//  disposable `HOME` whose profile extends PATH.

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
                 [ \"$1\" = -i ] && shift\n\
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

    /// A real bash login profile sources an rc with the usual interactive
    /// guard. Profile logging records startup reaching the profile, which a
    /// timed-out probe need not do, without using real dotfiles.
    fn bash_rc(&self, profile_setup: &str, interactive_setup: &str) {
        fs::write(
            self.home.join(".bash_profile"),
            format!(
                "export PATH={MINIMAL_PATH}\n\
                 printf '%s\\n' \"$-\" >> \"$HOME/probes\"\n\
                 {profile_setup}\n\
                 . \"$HOME/.bashrc\"\n"
            ),
        )
        .unwrap();
        fs::write(
            self.home.join(".bashrc"),
            format!(
                "case $- in *i*) ;; *) return ;; esac\n\
                 echo 'rc banner'\n\
                 {interactive_setup}\n"
            ),
        )
        .unwrap();
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
    assert_eq!(resolved.probe_mode, Some(LoginShellMode::InteractiveLogin));
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

/// Interactive bash ignores SIGTERM until it execs, so a probe whose deadline
/// fires while the shell is still starting must not wait out a termination
/// grace period: both probes stay within their budget.
#[test]
fn a_login_shell_ignoring_sigterm_is_still_bounded_by_the_probe_timeout() {
    let host = Host::new();
    let stubborn = host.fake_login_shell("sigterm-ignoring-login-sh", "trap '' TERM; sleep 30");

    let started = Instant::now();
    let resolved = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy::default(),
        &LoginShell::new(&stubborn, Duration::from_millis(300)),
    );
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert_eq!(resolved.source, ValidationEnvSource::LauncherFallback);
    let error = resolved.login_shell_error.as_deref().unwrap();
    assert!(
        error.contains("did not finish within 300ms") && error.contains("login fallback failed"),
        "{error}"
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

/// Toolchain setup behind bash's interactive guard is found by default;
/// opting out uses the old login-only environment with a separate cache entry.
#[test]
fn interactive_rc_toolchains_are_found_and_can_be_disabled() {
    let host = Host::new();
    host.bash_rc(
        "",
        &format!(
            "export PATH=\"{}:$PATH\"\nexport CARGO_HOME=\"$HOME/rc-cargo\"\n\
         export ORBIT_TEST_LOGIN_SECRET=secret",
            host.tools.display()
        ),
    );
    let bash = shell(Path::new("/bin/bash"));
    let interactive =
        ValidationEnvironment::resolve(host.launcher_env(), &ValidationEnvPolicy::default(), &bash);
    assert_eq!(
        interactive.probe_mode,
        Some(LoginShellMode::InteractiveLogin)
    );
    assert_eq!(interactive.fallback_reason, None);
    assert_eq!(interactive.path(), Some(host.login_path().as_str()));
    assert_eq!(
        var(&interactive, "CARGO_HOME"),
        Some(host.home.join("rc-cargo").to_str().unwrap())
    );
    assert_eq!(var(&interactive, "ORBIT_TEST_LOGIN_SECRET"), None);
    let (passed, output) = run_with(&interactive, "orbit-fake-tool-13987");
    assert!(passed, "interactive rc supplies the tool: {output}");

    let login = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy {
            interactive: false,
            ..ValidationEnvPolicy::default()
        },
        &bash,
    );
    assert_eq!(login.probe_mode, Some(LoginShellMode::Login));
    assert_eq!(login.fallback_reason, None);
    assert_eq!(login.path(), Some(MINIMAL_PATH));
    assert_eq!(var(&login, "CARGO_HOME"), None);
    assert!(!run_with(&login, "orbit-fake-tool-13987").0);
    assert_eq!(
        fs::read_to_string(host.home.join("probes"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

/// A broken interactive rc cannot prevent the login profile from providing
/// validation tooling. The failure and successful fallback are cached together.
/// A timeout can precede any shell marker. Record shell entry separately from
/// profile entry, and make a retry fail even if neither marker is reached.
#[test]
#[allow(
    clippy::print_stderr,
    reason = "native probe evidence records process IDs and observed startup modes"
)]
fn broken_interactive_rcs_fall_back_with_bounded_cached_outcomes() {
    for (rc, reason, timeout, stalled_startup) in [
        (
            "echo ready > \"$HOME/timeout-ready\"; exec /bin/sleep 30",
            "did not finish within 300ms",
            Duration::from_millis(300),
            false,
        ),
        (
            "echo 'rc failure' >&2; exit 7",
            "exited with status 7",
            Duration::from_secs(10),
            false,
        ),
        (
            "exec /bin/sh -c 'echo replaced-shell'",
            "marker missing",
            Duration::from_secs(10),
            false,
        ),
        (
            "echo ready > \"$HOME/timeout-ready\"; exec /bin/sleep 30",
            "did not finish within 300ms",
            Duration::from_millis(300),
            true,
        ),
    ] {
        let host = Host::new();
        host.bash_rc(
            &format!(
                "export PATH=\"{}:$PATH\"\nexport CARGO_HOME=\"$HOME/profile-cargo\"\n\
             export ORBIT_TEST_LOGIN_SECRET=secret",
                host.tools.display()
            ),
            rc,
        );
        let stall = if stalled_startup {
            // Stop before either observed-startup marker. The supervisor still
            // records the attempted child and kills it at the fixture deadline.
            "[ \"$1\" = -i ] && { echo ready > \"$HOME/timeout-ready\"; exec /bin/sleep 30; }\n"
        } else {
            ""
        };
        // Keep real native bash/profile startup in every row. Short deadline
        // rows advance the existing supervisor clock only after the failing
        // child reaches its stall; healthy startup does not spend that clock.
        let program = host.home.join("recording-bash");
        executable(
            &program,
            &format!(
                "#!/bin/sh\n\
             {stall}\
             printf '%s %s\\n' \"$$\" \"$1\" >> \"$HOME/shell-entries\"\n\
             exec /bin/bash \"$@\"\n"
            ),
        );
        let bash = LoginShell::new(&program, timeout);
        let started = Instant::now();
        let resolved = if timeout == Duration::from_millis(300) {
            resolve_with_ready_timeout(&host, &bash)
        } else {
            ValidationEnvironment::resolve(
                host.launcher_env(),
                &ValidationEnvPolicy::default(),
                &bash,
            )
        };
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "bounded fallback: {rc}"
        );
        assert_eq!(
            resolved.source,
            ValidationEnvSource::LoginShell,
            "{resolved:?}"
        );
        assert_eq!(resolved.probe_mode, Some(LoginShellMode::Login));
        assert_eq!(resolved.login_shell_error, None);
        let fallback = resolved.fallback_reason.as_deref().unwrap();
        assert!(fallback.contains(reason), "{fallback}");
        assert_eq!(resolved.path(), Some(host.login_path().as_str()));
        assert_eq!(
            var(&resolved, "CARGO_HOME"),
            Some(host.home.join("profile-cargo").to_str().unwrap())
        );
        assert_eq!(var(&resolved, "ORBIT_TEST_LOGIN_SECRET"), None);
        assert!(run_with(&resolved, "orbit-fake-tool-13987").0);
        let entries_path = host.home.join("shell-entries");
        let entry_log = fs::read_to_string(&entries_path).unwrap();
        let entries: Vec<bool> = entry_log
            .lines()
            .map(|line| line.split_whitespace().nth(1).unwrap() == "-i")
            .collect();
        if stalled_startup {
            assert_eq!(
                entries,
                vec![false],
                "timeout precedes the shell-entry marker"
            );
        } else {
            assert_eq!(entries, vec![true, false], "interactive then login entry");
        }
        let probes_path = host.home.join("probes");
        let probes = fs::read_to_string(&probes_path).unwrap();
        let modes: Vec<bool> = probes.lines().map(|flags| flags.contains('i')).collect();
        if stalled_startup {
            assert_eq!(modes, vec![false], "timeout precedes the profile marker");
        } else {
            assert_eq!(modes, vec![true, false], "rc failure is observed");
        }
        eprintln!(
            "rc={rc:?} stalled_startup={stalled_startup} elapsed={:?} \
             shell_entries={entry_log:?} profile_modes={modes:?} fallback={fallback:?}",
            started.elapsed()
        );
        // A marker alone cannot detect a retry killed before writing it. Make
        // either probe fail on a retry, so result equality also proves reuse
        // when startup never reaches the recording code. Keep the cache key.
        executable(
            &program,
            "#!/bin/sh\n\
             printf '%s %s\\n' \"$$\" \"$1\" >> \"$HOME/shell-entries\"\n\
             exit 91\n",
        );
        let cached = ValidationEnvironment::resolve(
            host.launcher_env(),
            &ValidationEnvPolicy::default(),
            &bash,
        );
        assert_eq!(cached, resolved);
        assert_eq!(
            fs::read_to_string(&entries_path).unwrap(),
            entry_log,
            "cached fallback must not launch either probe again"
        );
        assert_eq!(
            fs::read_to_string(&probes_path).unwrap(),
            probes,
            "cached fallback must not re-enter the profile or rc"
        );
    }
}

/// The 300ms budget tests timeout ownership, not how quickly a shared host can
/// schedule two shell startups. Real processes and real cleanup remain; the
/// existing thread-scoped clock makes only the interactive stall expire.
fn resolve_with_ready_timeout(host: &Host, bash: &LoginShell) -> ValidationEnvironment {
    let origin = Instant::now();
    let now = Rc::new(Cell::new(origin));
    let clock = Rc::clone(&now);
    let children = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&children);
    let ready = host.home.join("timeout-ready");
    let seam = SupervisorSeam {
        wait: Some(Box::new(move |child| {
            let mut pids = observed.borrow_mut();
            if pids.last() != Some(&child.id()) {
                pids.push(child.id());
            }
            if pids.len() == 1 && ready.exists() {
                now.set(now.get() + Duration::from_millis(300));
                return Ok(None);
            }
            // Keep an independent wall bound even if startup never reaches
            // readiness. Advancing expires the probe through real cleanup.
            if origin.elapsed() >= Duration::from_secs(5) {
                now.set(now.get() + Duration::from_secs(10));
                return Ok(None);
            }
            child.try_wait()
        })),
        ..SupervisorSeam::default()
    };
    let resolved = with_supervision_clock(
        move || clock.get(),
        || {
            with_supervisor_seam(seam, || {
                ValidationEnvironment::resolve(
                    host.launcher_env(),
                    &ValidationEnvPolicy::default(),
                    bash,
                )
            })
        },
    );
    assert_eq!(children.borrow().len(), 2, "exactly two probes attempted");
    for &pid in children.borrow().iter() {
        // Safety: signal zero only checks existence; each child was observed
        // through its owned handle and must have been reaped before return.
        let exists = unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) };
        assert_eq!(exists, -1, "probe {pid} must not survive resolution");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    resolved
}

/// A shell that cannot start records both attempts; a cached failure does not
/// unexpectedly retry a newly repaired program during the cache window.
#[test]
fn startup_failures_are_recorded_and_cached() {
    let host = Host::new();
    let program = host.home.join("missing-shell");
    let probe = shell(&program);
    let failed = ValidationEnvironment::resolve(
        host.launcher_env(),
        &ValidationEnvPolicy::default(),
        &probe,
    );
    assert_eq!(failed.source, ValidationEnvSource::LauncherFallback);
    assert_eq!(failed.probe_mode, None);
    let error = failed.login_shell_error.as_deref().unwrap();
    assert!(
        error.contains("could not start") && error.contains("login fallback failed"),
        "{error}"
    );
    executable(&program, "#!/bin/sh\nexit 9\n");
    assert_eq!(
        ValidationEnvironment::resolve(
            host.launcher_env(),
            &ValidationEnvPolicy::default(),
            &probe,
        ),
        failed
    );
}

/// Doctor's candidate enumeration follows executable PATH order while
/// ignoring non-executable files, duplicate entries and symlink aliases.
#[test]
fn tool_locations_distinguish_shadowed_executables_from_aliases() {
    let host = Host::new();
    let later = host.home.join("later");
    let alias = host.home.join("alias");
    fs::create_dir_all(&later).unwrap();
    std::os::unix::fs::symlink(&host.tools, &alias).unwrap();
    executable(&host.tools.join("python3"), "#!/bin/sh\necho first\n");
    executable(&later.join("python3"), "#!/bin/sh\necho later\n");
    fs::write(host.tools.join("git"), "not executable").unwrap();
    let environment = ValidationEnvironment::launcher(vec![(
        "PATH".to_string(),
        format!(
            "{}:{}:{}:{}",
            host.tools.display(),
            alias.display(),
            host.tools.display(),
            later.display()
        ),
    )]);
    assert_eq!(
        environment.program_paths("python3"),
        vec![host.tools.join("python3"), later.join("python3")]
    );
    assert!(environment.program_paths("git").is_empty());
    let (passed, output) = run_with(&environment, "python3");
    assert!(passed, "{output}");
    assert_eq!(output.trim(), "first");
}

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
