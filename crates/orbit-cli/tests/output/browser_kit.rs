//! `orbit browser-kit status`, its `orbit doctor` row and the `workspace init`
//! offer over fixture kits in a disposable HOME. Nothing here downloads:
//! each fixture lays out the files a real kit has.
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
