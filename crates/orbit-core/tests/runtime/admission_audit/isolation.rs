//! Observe the native probe's isolation path from the test-command boundary.

use std::io::Write;
use std::process::Command;

use orbit_common::test_env::{INHERITED_AUTHORITY_ENV, SCRUBBED_MARKER_ENV};

const SCENARIO: &str = "ORBIT_TEST_ISOLATION_SCENARIO";
const TEST: &str = "isolated_diagnostics_reach_the_test_command";
const NOTE: &str = "test-note: isolated probe diagnostic";

#[test]
fn isolated_diagnostics_reach_the_test_command() {
    if let Ok(scenario) = std::env::var(SCENARIO) {
        let child = match scenario.as_str() {
            "missing" => "missing_isolation_entry_point",
            "ignored" => "ignored_isolation_entry_point",
            _ => TEST,
        };
        if !super::isolate(module_path!(), child) {
            return;
        }
        for key in INHERITED_AUTHORITY_ENV {
            assert!(std::env::var_os(key).is_none(), "child retained {key}");
        }
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(
            std::env::current_dir().unwrap(),
            home.canonicalize().unwrap()
        );
        assert_eq!(std::env::var_os("USERPROFILE").unwrap(), home.as_os_str());
        assert_ne!(scenario, "failed", "intentional isolated child failure");
        writeln!(std::io::stderr(), "{NOTE}").unwrap();
        return;
    }

    let exact_test = format!("admission_audit::isolation::{TEST}");
    for (scenario, nocapture) in [
        ("passed", true),
        ("passed", false),
        ("failed", true),
        ("missing", true),
        ("ignored", true),
    ] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", &exact_test, "--test-threads=1"]);
        if nocapture {
            command.arg("--nocapture");
        }
        // Keep the binary's startup scrub from masking a broken isolation
        // helper: only isolate itself may remove this observer's authority.
        command.env(SCRUBBED_MARKER_ENV, "1");
        for key in INHERITED_AUTHORITY_ENV {
            command.env(key, "inherited-isolation-authority");
        }
        let output = command
            .env_remove("ORBIT_TEST_TASK_FIXTURE_CHILD")
            .env(SCENARIO, scenario)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            scenario == "passed",
            "scenario={scenario}, nocapture={nocapture}:\n{stdout}\n{stderr}"
        );
        if scenario == "passed" {
            assert!(
                stderr.lines().any(|line| line == NOTE),
                "successful isolated diagnostic lost at the test-command boundary \
                 (nocapture={nocapture}):\n{stdout}\n{stderr}"
            );
        } else {
            let reason = match scenario {
                "failed" => "intentional isolated child failure",
                "missing" => "missing_isolation_entry_point",
                "ignored" => "ignored_isolation_entry_point",
                _ => unreachable!(),
            };
            assert!(stderr.contains(reason), "{stdout}\n{stderr}");
        }
    }
}

#[test]
#[ignore = "negative control: isolation must reject an ignored child"]
fn ignored_isolation_entry_point() {
    panic!("ignored child must never execute");
}
