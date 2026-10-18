//! Browser-kit CLI reports, onboarding and install recovery in a disposable
//! HOME. Fixtures lay out the kit and stub external installer commands;
//! nothing here downloads packages or runs a real browser.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::bootstrap::browser_kit::{KitLayout, PLAYWRIGHT_VERSION, WORKER_ENV_NAMES};
use serde_json::{Value, json};

use crate::git_repo::WorkCheckout;

const REVISION: &str = "1248";

/// A kit on disk: the installed Playwright version, whether its browser
/// revision is downloaded, and the version a passing `verify` recorded.
struct FixtureKit {
    version: &'static str,
    browser_installed: bool,
    verified_version: Option<&'static str>,
}

fn write_kit(home: &Path, kit: &FixtureKit) -> PathBuf {
    let layout = KitLayout::for_home(home);
    let root = layout.root().to_path_buf();
    let package = root.join("kit/node_modules/playwright");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        json!({ "name": "playwright", "version": kit.version }).to_string(),
    )
    .unwrap();
    fs::write(package.join("index.mjs"), "export {};\n").unwrap();
    let core = root.join("kit/node_modules/playwright-core");
    fs::create_dir_all(&core).unwrap();
    fs::write(
        core.join("browsers.json"),
        json!({ "browsers": [{ "name": "chromium-headless-shell", "revision": REVISION }] })
            .to_string(),
    )
    .unwrap();
    let browsers = root.join("browsers");
    fs::create_dir_all(&browsers).unwrap();
    if kit.browser_installed {
        let shell = browsers.join(format!("chromium_headless_shell-{REVISION}"));
        fs::create_dir_all(&shell).unwrap();
        fs::write(shell.join("INSTALLATION_COMPLETE"), "").unwrap();
    }
    fs::write(root.join("env.sh"), "# written by the operator\n").unwrap();
    if let Some(version) = kit.verified_version {
        fs::write(
            root.join("orbit-kit.json"),
            json!({
                "schema_version": 1,
                "playwright_version": version,
                "chromium_revision": REVISION,
                "browser_version": "156.0.8078.4",
                "verified_at": "2026-10-18T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();
    }
    root
}

fn orbit(fixture: &WorkCheckout) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&fixture.work)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env_remove("ORBIT_FORMAT")
        .env("RUST_LOG", "off");
    command
}

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("JSON stdout: {error}; {output:?}"))
}

fn doctor_row(fixture: &WorkCheckout) -> Value {
    let output = orbit(fixture).args(["doctor", "--json"]).output().unwrap();
    let rows = json_stdout(&output);
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row["check"] == "browser-kit"))
        .cloned()
        .unwrap_or_else(|| panic!("browser-kit doctor row: {rows}"))
}

/// Each kit state reaches `status` and `doctor` with the command that fixes
/// it, and only a kit verified at its installed version is handed to workers.
#[test]
fn status_and_doctor_report_each_kit_state_with_its_fix() {
    struct Case {
        name: &'static str,
        kit: Option<FixtureKit>,
        state: &'static str,
        fixes: &'static [&'static str],
        doctor: &'static str,
    }
    let cases = [
        Case {
            name: "absent",
            kit: None,
            state: "not_installed",
            fixes: &[],
            doctor: "info",
        },
        Case {
            name: "pinned and verified",
            kit: Some(FixtureKit {
                version: PLAYWRIGHT_VERSION,
                browser_installed: true,
                verified_version: Some(PLAYWRIGHT_VERSION),
            }),
            state: "verified",
            fixes: &[],
            doctor: "ok",
        },
        Case {
            name: "pinned, never verified",
            kit: Some(FixtureKit {
                version: PLAYWRIGHT_VERSION,
                browser_installed: true,
                verified_version: None,
            }),
            state: "unverified",
            fixes: &["orbit browser-kit verify"],
            doctor: "warning",
        },
        Case {
            name: "mismatched version, verification from the old version",
            kit: Some(FixtureKit {
                version: "1.0.0",
                browser_installed: true,
                verified_version: Some(PLAYWRIGHT_VERSION),
            }),
            state: "unverified",
            fixes: &["orbit browser-kit install"],
            doctor: "warning",
        },
        Case {
            name: "missing browser revision",
            kit: Some(FixtureKit {
                version: PLAYWRIGHT_VERSION,
                browser_installed: false,
                verified_version: Some(PLAYWRIGHT_VERSION),
            }),
            state: "incomplete",
            fixes: &["orbit browser-kit install"],
            doctor: "warning",
        },
    ];
    for case in cases {
        let name = case.name;
        let fixture = WorkCheckout::new();
        fs::create_dir_all(&fixture.work).unwrap();
        if let Some(kit) = &case.kit {
            write_kit(&fixture.home, kit);
        }

        let output = orbit(&fixture)
            .args(["browser-kit", "status", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{name}: {output:?}");
        let status = json_stdout(&output);
        assert_eq!(status["state"], case.state, "{name}: {status}");
        let verified = case.state == "verified";
        assert_eq!(status["verified"], verified, "{name}: {status}");
        let worker_env: Vec<&str> = if verified {
            WORKER_ENV_NAMES.to_vec()
        } else {
            Vec::new()
        };
        assert_eq!(status["worker_env"], json!(worker_env), "{name}: {status}");
        let problems = status["problems"].as_array().expect("problems");
        assert_eq!(
            problems.is_empty(),
            case.fixes.is_empty(),
            "{name}: {status}"
        );
        for fix in case.fixes {
            assert!(
                problems
                    .iter()
                    .any(|problem| problem["fix"].as_str().is_some_and(|f| f.starts_with(fix))),
                "{name}: expected the fix `{fix}`: {status}"
            );
        }

        let row = doctor_row(&fixture);
        assert_eq!(row["status"], case.doctor, "{name}: {row}");
        let remediation = row["remediation"].as_str().unwrap_or_default();
        if case.kit.is_none() {
            assert!(
                remediation.contains("orbit browser-kit install"),
                "{name}: {row}"
            );
        }
        for fix in case.fixes {
            assert!(remediation.contains(fix), "{name}: {row}");
        }
    }
}

/// The install, verify and remove steps refuse to run inside a worker, before
/// touching the kit or the network.
#[test]
fn kit_changes_are_refused_inside_a_worker() {
    let fixture = WorkCheckout::new();
    fs::create_dir_all(&fixture.work).unwrap();
    let root = write_kit(
        &fixture.home,
        &FixtureKit {
            version: PLAYWRIGHT_VERSION,
            browser_installed: true,
            verified_version: None,
        },
    );
    for args in [
        &["browser-kit", "install"][..],
        &["browser-kit", "verify"],
        &["browser-kit", "remove", "--confirm"],
    ] {
        let output = orbit(&fixture)
            .env("ORBIT_WORKER_CONTEXT_REQUIRED", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("never inside a worker"),
            "{args:?}: {output:?}"
        );
    }
    assert!(root.join("env.sh").is_file(), "the kit is untouched");
    assert!(
        !root.join("orbit-kit.json").exists(),
        "nothing was verified"
    );
}

#[cfg(target_os = "linux")]
mod verification_errors {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// Refuse process creation only in this CLI child. Threads remain available
    /// for logging; clone3 falls back to clone so its flags can be inspected.
    fn deny_probe_spawn(command: &mut Command) {
        let instruction = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
        let deny = libc::SECCOMP_RET_ERRNO | libc::EACCES as u32;
        let mut filter = vec![instruction(0x20, 0, 0, 0)]; // seccomp_data.nr
        #[cfg(target_arch = "x86_64")]
        for syscall in [libc::SYS_fork, libc::SYS_vfork] {
            filter.push(instruction(0x15, 0, 1, syscall as u32));
            filter.push(instruction(0x06, 0, 0, deny));
        }
        filter.extend([
            instruction(0x15, 0, 1, libc::SYS_clone3 as u32),
            instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
            instruction(0x15, 1, 0, libc::SYS_clone as u32),
            instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
            instruction(0x20, 0, 0, 16), // low word of clone's flags
            instruction(0x45, 1, 0, libc::CLONE_THREAD as u32),
            instruction(0x06, 0, 0, deny),
            instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
        ]);
        // SAFETY: only libc syscalls run after fork, and the kernel copies the
        // owned filter synchronously. Restrictions affect only this child.
        unsafe {
            command.pre_exec(move || {
                let program = libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_ptr().cast_mut(),
                };
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// A failed spawn must revoke an earlier pass through the real CLI, and a
    /// subsequent successful smoke probe must restore the worker handoff.
    #[test]
    fn probe_spawn_error_revokes_previous_pass_and_success_restores_it() {
        if crate::isolated_test::run_in_child(
            "browser_kit::verification_errors::probe_spawn_error_revokes_previous_pass_and_success_restores_it",
        ) {
            return;
        }
        let fixture = WorkCheckout::new();
        fs::create_dir_all(&fixture.work).unwrap();
        let root = write_kit(
            &fixture.home,
            &FixtureKit {
                version: PLAYWRIGHT_VERSION,
                browser_installed: true,
                verified_version: Some(PLAYWRIGHT_VERSION),
            },
        );
        let bin = fixture.work.join("bin");
        fs::create_dir(&bin).unwrap();
        let node = bin.join("node");
        fs::write(
            &node,
            "#!/bin/sh\n: > \"$ORBIT_BROWSER_KIT_SCREENSHOT\"\nprintf '{\"text\":\"kit ok\",\"version\":\"fixture\"}\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
        let status = || {
            let output = orbit(&fixture)
                .args(["browser-kit", "status", "--json"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            json_stdout(&output)
        };
        assert_eq!(status()["verified"], true);
        assert_eq!(
            orbit_core::bootstrap::browser_kit::worker_env_for_home(&fixture.home).len(),
            WORKER_ENV_NAMES.len()
        );

        let mut denied = Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            denied.env_remove(name);
        });
        denied
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env("PATH", &bin)
            .env("RUST_LOG", "off")
            .env_remove("ORBIT_FORMAT")
            .args(["browser-kit", "verify", "--json"]);
        deny_probe_spawn(&mut denied);
        let output = assert_cmd::Command::from_std(denied)
            .timeout(test_env::FIXTURE_STEP_DEADLINE)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&std::io::Error::from_raw_os_error(libc::EACCES).to_string()),
            "the original spawn diagnostic must survive: {output:?}"
        );
        assert!(
            root.join("verify").is_dir(),
            "verification reached the probe"
        );
        assert!(
            !root.join("orbit-kit.json").exists(),
            "failed probe kept a pass"
        );
        let failed = status();
        assert_eq!(failed["state"], "unverified", "{failed}");
        assert_eq!(failed["verified"], false, "{failed}");
        assert_eq!(failed["worker_env"], json!([]), "{failed}");
        assert!(orbit_core::bootstrap::browser_kit::worker_env_for_home(&fixture.home).is_empty());

        let output = orbit(&fixture)
            .env("PATH", &bin)
            .args(["browser-kit", "verify", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(json_stdout(&output)["passed"], true);
        let record: Value =
            serde_json::from_slice(&fs::read(root.join("orbit-kit.json")).unwrap()).unwrap();
        assert_eq!(record["playwright_version"], PLAYWRIGHT_VERSION);
        assert_eq!(record["chromium_revision"], REVISION);
        assert_eq!(record["browser_version"], "fixture");
        let passed = status();
        assert_eq!(passed["state"], "verified", "{passed}");
        assert_eq!(passed["worker_env"], json!(WORKER_ENV_NAMES), "{passed}");
        assert_eq!(
            orbit_core::bootstrap::browser_kit::worker_env_for_home(&fixture.home).len(),
            WORKER_ENV_NAMES.len()
        );
    }
}

/// Onboarding a repository with a web UI offers the kit but, with no
/// terminal and no `--install-browser-kit`, installs nothing; a verified kit
/// is reported ready.
#[test]
fn workspace_init_offers_the_kit_for_a_web_ui_without_installing_it() {
    if crate::isolated_test::run_in_child(
        "browser_kit::workspace_init_offers_the_kit_for_a_web_ui_without_installing_it",
    ) {
        return;
    }
    let init = |fixture: &WorkCheckout| {
        let output = orbit(fixture)
            .args(["workspace", "init", "--name", "kit-offer", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        json_stdout(&output)["browser_kit"].clone()
    };

    let plain = WorkCheckout::new();
    crate::git_repo::init(&plain.work);
    assert_eq!(init(&plain)["status"], "no_web_ui");

    let web = WorkCheckout::new();
    crate::git_repo::init(&web.work);
    fs::create_dir_all(web.work.join("site")).unwrap();
    fs::write(web.work.join("site/index.html"), "<h1>hi</h1>\n").unwrap();
    let offer = init(&web);
    assert_eq!(offer["status"], "offered", "{offer}");
    assert_eq!(offer["web_ui"], "site/index.html", "{offer}");
    assert_eq!(offer["next"], "orbit browser-kit install", "{offer}");
    assert!(
        !KitLayout::for_home(&web.home).root().exists(),
        "init installed a kit nobody asked for"
    );

    let ready = WorkCheckout::new();
    crate::git_repo::init(&ready.work);
    fs::write(ready.work.join("package.json"), "{}\n").unwrap();
    write_kit(
        &ready.home,
        &FixtureKit {
            version: PLAYWRIGHT_VERSION,
            browser_installed: true,
            verified_version: Some(PLAYWRIGHT_VERSION),
        },
    );
    let kit = init(&ready);
    assert_eq!(kit["status"], "ready", "{kit}");
    assert_eq!(kit["web_ui"], "package.json", "{kit}");
}

#[cfg(target_os = "linux")]
mod library_recovery {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(bin: &Path, name: &str, body: &str) {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Real CLI installs with deterministic package-manager and Node stand-ins:
    /// cached downloads survive interruption/failure and only extracted packages
    /// enter the report. No network, system package manager or browser is run.
    #[test]
    fn install_recovers_cached_archives_and_reports_successful_extractions() {
        if crate::isolated_test::run_in_child(
            "browser_kit::library_recovery::install_recovers_cached_archives_and_reports_successful_extractions",
        ) {
            return;
        }
        let release = fs::read_to_string("/etc/os-release").unwrap_or_default();
        if !release.lines().any(|line| {
            line.split_once('=').is_some_and(|(key, value)| {
                matches!(key, "ID" | "ID_LIKE")
                    && value
                        .trim_matches(['\"', '\''])
                        .split_whitespace()
                        .any(|id| matches!(id, "debian" | "ubuntu"))
            })
        }) {
            #[allow(
                clippy::print_stderr,
                reason = "the test gate must see when this host lacks the Debian/Ubuntu install path"
            )]
            {
                eprintln!("SKIP: no-root browser-kit package recovery requires Debian or Ubuntu");
            }
            return;
        }

        for case in ["cached", "failed-extraction", "fresh", "unavailable"] {
            let fixture = WorkCheckout::new();
            fs::create_dir_all(&fixture.work).unwrap();
            let root = write_kit(
                &fixture.home,
                &FixtureKit {
                    version: PLAYWRIGHT_VERSION,
                    browser_installed: true,
                    verified_version: None,
                },
            );
            let layout = KitLayout::for_home(&fixture.home);
            let shell = layout.browsers().join(format!(
                "chromium_headless_shell-{REVISION}/chrome-headless-shell-linux64"
            ));
            fs::create_dir_all(&shell).unwrap();
            fs::write(shell.join("chrome-headless-shell"), "fixture").unwrap();
            fs::create_dir_all(layout.debs()).unwrap();
            let archive = layout.debs().join("libneeded_1_amd64.deb");
            if case == "cached" {
                fs::write(&archive, "cached package\n").unwrap();
            }
            let unrelated = layout.debs().join("libunrelated_1_amd64.deb");
            fs::write(&unrelated, "unrelated cache\n").unwrap();
            let bin = fixture.work.join("bin");
            fs::create_dir(&bin).unwrap();
            executable(
                &bin,
                "node",
                r#"
case "$2" in
  install-deps) printf 'Missing system dependencies (1):\n  libneeded\n' ;;
  -e) : > "$ORBIT_BROWSER_KIT_SCREENSHOT"
      printf '{"text":"kit ok","version":"fixture"}\n' ;;
  *) exit 90 ;;
esac"#,
            );
            executable(
                &bin,
                "apt-get",
                r#"
[ "$1" = download ] && [ "$2" = libneeded ]
echo download >> "$KIT_TEST_ROOT/calls"
[ "$KIT_TEST_CASE" != unavailable ] || exit 1
[ -f libneeded_1_amd64.deb ] || printf 'cached package\n' > libneeded_1_amd64.deb"#,
            );
            executable(
                &bin,
                "dpkg-deb",
                r#"
[ "$1" = -x ]
case "$2" in */libneeded_1_amd64.deb) ;; *) exit 91 ;; esac
echo extract >> "$KIT_TEST_ROOT/calls"
if [ "$KIT_TEST_FAIL" = 1 ]; then echo 'fixture extraction failed' >&2; exit 1; fi
mkdir -p "$3/usr/lib/x86_64-linux-gnu"
: > "$3/usr/lib/x86_64-linux-gnu/libneeded.so""#,
            );
            executable(
                &bin,
                "ldd",
                r#"
case ":${LD_LIBRARY_PATH:-}:" in
  *":$KIT_TEST_ROOT/root/usr/lib/x86_64-linux-gnu:"*)
    [ ! -f "$KIT_TEST_ROOT/root/usr/lib/x86_64-linux-gnu/libneeded.so" ] || exit 0 ;;
esac
echo 'libneeded.so => not found'"#,
            );
            executable(&bin, "fc-list", "echo fixture-font");
            executable(&bin, "dpkg-query", "exit 1");
            for name in ["npm", "sudo", "apt-cache"] {
                executable(
                    &bin,
                    name,
                    "echo unexpected >> \"$KIT_TEST_ROOT/calls\"; exit 92",
                );
            }
            let install = |fail: bool| {
                orbit(&fixture)
                    .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                    .env("KIT_TEST_ROOT", &root)
                    .env("KIT_TEST_CASE", case)
                    .env("KIT_TEST_FAIL", if fail { "1" } else { "0" })
                    .args(["browser-kit", "install", "--json"])
                    .output()
                    .unwrap()
            };
            if case == "failed-extraction" {
                let failed = install(true);
                assert!(!failed.status.success(), "{case}: {failed:?}");
                assert!(
                    String::from_utf8_lossy(&failed.stderr).contains("fixture extraction failed"),
                    "{failed:?}"
                );
                assert!(!layout.verification_file().exists());
                assert!(
                    failed.stdout.is_empty(),
                    "failed extraction emits an error, not an install report: {failed:?}"
                );
                assert_eq!(fs::read_to_string(&archive).unwrap(), "cached package\n");
            }
            let output = install(false);
            let report = json_stdout(&output);
            if case == "unavailable" {
                assert!(!output.status.success(), "{case}: {output:?}");
                assert_eq!(report["libraries"]["unpacked_packages"], json!([]));
                assert_eq!(report["libraries"]["unresolved"], json!(["libneeded.so"]));
                assert_eq!(report["verification"], Value::Null);
            } else {
                assert!(output.status.success(), "{case}: {output:?}");
                assert_eq!(
                    report["libraries"]["unpacked_packages"],
                    json!(["libneeded"])
                );
                assert_eq!(report["libraries"]["unresolved"], json!([]));
                assert_eq!(report["status"]["verified"], true);
                assert_eq!(fs::read_to_string(&archive).unwrap(), "cached package\n");
                let calls = fs::read_to_string(root.join("calls")).unwrap();
                let expected = if case == "failed-extraction" {
                    "download\nextract\ndownload\nextract\n"
                } else {
                    "download\nextract\n"
                };
                assert_eq!(calls, expected, "{case}");
                let adopted = install(false);
                assert!(adopted.status.success(), "{case}: {adopted:?}");
                assert_eq!(
                    json_stdout(&adopted)["libraries"]["unpacked_packages"],
                    json!([])
                );
                assert_eq!(
                    fs::read_to_string(root.join("calls")).unwrap(),
                    calls,
                    "ready kits skip package work"
                );
            }
            assert_eq!(fs::read_to_string(&unrelated).unwrap(), "unrelated cache\n");
            assert_eq!(
                fs::read_to_string(root.join("env.sh")).unwrap(),
                "# written by the operator\n"
            );
        }
    }
}
