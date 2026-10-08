//! Global shorthand behavior at the built-binary boundary. State is created
//! only by isolated CLI children, with inherited run authority removed.

use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;

fn run(checkout: &crate::git_repo::WorkCheckout, args: &[&str]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&checkout.work)
        .env("HOME", &checkout.home)
        .env("USERPROFILE", &checkout.home)
        .env_remove("ORBIT_FORMAT");
    let bin = checkout.home.join("bin");
    if bin.exists() {
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env("PATH", std::env::join_paths(paths).expect("fixture PATH"));
    }
    command.args(args).output().expect("run isolated orbit")
}

#[test]
fn newly_covered_commands_emit_one_json_document_at_every_flag_position() {
    let checkout = crate::git_repo::WorkCheckout::new();
    // The output-option contract is independent of whether the host has a
    // queryable user scheduler. Isolated children see an absent native unit.
    #[cfg(unix)]
    install_native_clock_fixture(&checkout);
    let initialized = run(&checkout, &["workspace", "init", "--name", "global-json"]);
    assert!(
        initialized.status.success(),
        "workspace init: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let mut commands = vec![
        &["workspace", "list"][..],
        &["workspace", "show"],
        &["config", "path"],
    ];
    // The native scheduler backend requires Unix commands. On other hosts
    // clock's --json acceptance is covered by the assembled-tree parser test.
    if cfg!(unix) {
        commands.push(&["clock", "status"]);
    }
    for command in commands {
        for position in 0..=command.len() {
            let mut args = command.to_vec();
            args.insert(position, "--json");
            let output = run(&checkout, &args);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let document: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|error| panic!("{args:?} must emit one JSON document: {error}"));
            assert!(document.is_object() || document.is_array(), "{args:?}");
        }
    }
}

#[cfg(unix)]
fn install_native_clock_fixture(checkout: &crate::git_repo::WorkCheckout) {
    use std::os::unix::fs::PermissionsExt;

    let bin = checkout.home.join("bin");
    std::fs::create_dir_all(&bin).expect("native manager fixture directory");
    for (name, script) in [
        (
            "systemctl",
            "#!/bin/sh\ncase \"$2\" in\nis-enabled) printf 'disabled\\n'; exit 1;;\nshow) printf 'LoadState=not-found\\nActiveState=inactive\\n'; exit 0;;\n*) exit 1;;\nesac\n",
        ),
        (
            "launchctl",
            "#!/bin/sh\nprintf 'Could not find service\\n' >&2\nexit 113\n",
        ),
    ] {
        let path = bin.join(name);
        std::fs::write(&path, script).expect("write native manager fixture");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("executable native manager fixture");
    }
}

#[test]
fn conflicting_output_formats_are_json_usage_errors_across_command_levels() {
    let checkout = crate::git_repo::WorkCheckout::new();
    for args in [
        &["task", "list", "--json", "--format", "table"][..],
        &["--json", "task", "list", "--format", "table"],
        &["--format", "table", "task", "--json", "list"],
        &["task", "list", "--format=ndjson", "--json"],
        &["task", "list", "--json", "--format", "auto"],
    ] {
        let output = run(&checkout, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let error: Value = serde_json::from_slice(&output.stderr)
            .unwrap_or_else(|error| panic!("{args:?} must emit a JSON usage error: {error}"));
        assert_eq!(error["code"], "usage_error", "{args:?}");
        let message = error["error"].as_str().expect("usage error message");
        assert!(message.contains("--json") && message.contains("--format"));
    }
}

#[test]
fn json_shorthand_preserves_the_local_audit_export_file_format() {
    let checkout = crate::git_repo::WorkCheckout::new();
    let path = checkout.work.join("audit.csv");
    let output = run(
        &checkout,
        &[
            "--json",
            "audit",
            "export",
            "--format",
            "csv",
            "--output",
            path.to_str().expect("export path"),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut csv = csv::Reader::from_path(path).expect("CSV export");
    assert!(
        csv.headers()
            .expect("CSV header")
            .iter()
            .any(|name| name == "command")
    );
    // This file-producing command retains its existing human confirmation.
    assert!(
        String::from_utf8(output.stdout)
            .expect("export confirmation")
            .starts_with("Exported ")
    );
}
