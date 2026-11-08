//! Script steps phase 3 at the runtime's grant and script-step boundaries:
//! `orbit script grant` refuses a root that fails its probe, and a granted
//! root that is missing at run time refuses the step before anything is
//! created or spawned with `script_root_unavailable`, naming the path. On
//! macOS, `strict` worker containment refuses every script step.

use std::path::PathBuf;

use orbit_core::OrbitRuntime;
use orbit_core::runtime::scripts::{ScriptGrantOptions, ScriptGrantState};
use orbit_engine::{RuntimeHost, ScriptStepRequest, ScriptStepRun};
use orbit_types::workflow::activity_job::script_refusal;
use serde_json::json;

struct Fixture {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    outbox: PathBuf,
    /// Written by the entry when it runs; must stay absent after a refusal.
    ran_marker: PathBuf,
}

/// A workspace with an ungranted `export` script that writes into its
/// granted `outbox` write root, with `global_config` as the host config.
fn fixture(global_config: &str) -> Fixture {
    crate::scenarios::require_isolated_mutable_fixture("script_roots::fixture");
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let outbox = root.path().join("outbox");
    for dir in [&global, &repo.join(".orbit/resources/scripts"), &outbox] {
        std::fs::create_dir_all(dir).unwrap();
    }
    if !global_config.is_empty() {
        std::fs::write(global.join("config.toml"), global_config).unwrap();
    }
    let outbox = outbox.canonicalize().unwrap();
    let ran_marker = outbox.join("ran");
    std::fs::write(
        repo.join("export.sh"),
        format!(
            "#!/bin/sh\ntrue > '{}'\nprintf '{{}}'\n",
            ran_marker.display()
        ),
    )
    .unwrap();
    std::fs::write(
        repo.join(".orbit/resources/scripts/export.yaml"),
        "schemaVersion: 1\nkind: Script\nmetadata:\n  name: export\nspec:\n  \
         entry: export.sh\n  interpreter: [sh]\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    Fixture {
        _root: root,
        runtime,
        outbox,
        ran_marker,
    }
}

fn grant_outbox(fixture: &Fixture, outbox: PathBuf) -> Result<(), orbit_core::OrbitError> {
    fixture
        .runtime
        .grant_script(
            "export",
            &ScriptGrantOptions {
                write_roots: vec![outbox],
                ..Default::default()
            },
        )
        .map(|_| ())
}

fn request() -> ScriptStepRequest {
    ScriptStepRequest {
        run_id: "run-roots".to_string(),
        step_id: "export".to_string(),
        script: "export".to_string(),
        input: json!({}),
        workspace_path: None,
        timeout_seconds: Some(60),
    }
}

#[test]
fn a_grant_refuses_a_root_that_fails_its_probe() {
    if !super::dispatch_admission::isolated(
        "script_roots::a_grant_refuses_a_root_that_fails_its_probe",
    ) {
        return;
    }
    let fixture = fixture("");
    let missing = fixture.outbox.join("not-yet");

    let error = grant_outbox(&fixture, missing.clone())
        .expect_err("a write root that does not exist is refused at grant");

    let message = error.to_string();
    assert!(
        message.contains(&missing.display().to_string()) && message.contains("does not exist"),
        "{message}"
    );
    let status = fixture
        .runtime
        .script_status("export")
        .unwrap()
        .expect("the script is listed");
    assert_eq!(
        status.state,
        ScriptGrantState::Ungranted,
        "nothing was granted"
    );
}

#[test]
fn a_granted_root_missing_at_run_time_refuses_before_spawn() {
    if !super::dispatch_admission::isolated(
        "script_roots::a_granted_root_missing_at_run_time_refuses_before_spawn",
    ) {
        return;
    }
    let fixture = fixture("");
    grant_outbox(&fixture, fixture.outbox.clone()).expect("an existing write root is granted");
    std::fs::remove_dir(&fixture.outbox).unwrap();

    let ScriptStepRun::Refused {
        code,
        message,
        record,
    } = RuntimeHost::run_script_step(&fixture.runtime, &request())
    else {
        panic!("a step whose granted root is gone must refuse");
    };

    assert_eq!(code, script_refusal::ROOT_UNAVAILABLE, "{message}");
    assert!(
        message.contains(&fixture.outbox.display().to_string()),
        "the refusal names the root: {message}"
    );
    assert!(
        record.argv.is_empty() && record.profile.is_none(),
        "refused before the profile was probed or anything spawned: {record:?}"
    );
    assert!(
        !fixture.outbox.exists(),
        "the job's process does not create a missing root"
    );
    assert!(!fixture.ran_marker.exists(), "nothing ran");
}

#[cfg(unix)]
fn mkfifo(path: &std::path::Path) {
    let c_path = std::ffi::CString::new(path.as_os_str().to_string_lossy().into_owned()).unwrap();
    // SAFETY: `c_path` is a valid NUL-terminated path for the call.
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(
        rc,
        0,
        "mkfifo {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
#[test]
fn a_grant_refuses_a_fifo_read_root_and_grants_nothing() {
    if !super::dispatch_admission::isolated(
        "script_roots::a_grant_refuses_a_fifo_read_root_and_grants_nothing",
    ) {
        return;
    }
    let fixture = fixture("");
    let fifo = fixture._root.path().join("fifo");
    mkfifo(&fifo);

    let error = fixture
        .runtime
        .grant_script(
            "export",
            &ScriptGrantOptions {
                read_roots: vec![fifo.clone()],
                ..Default::default()
            },
        )
        .expect_err("a FIFO read root is refused at grant");

    let message = error.to_string();
    assert!(
        message.contains(&fifo.display().to_string())
            && message.contains("not a regular file or directory"),
        "{message}"
    );
    let status = fixture
        .runtime
        .script_status("export")
        .unwrap()
        .expect("the script is listed");
    assert_eq!(
        status.state,
        ScriptGrantState::Ungranted,
        "nothing was granted"
    );
}

#[cfg(unix)]
#[test]
fn a_granted_read_root_replaced_with_a_fifo_refuses_before_spawn() {
    if !super::dispatch_admission::isolated(
        "script_roots::a_granted_read_root_replaced_with_a_fifo_refuses_before_spawn",
    ) {
        return;
    }
    let fixture = fixture("");
    let data = fixture._root.path().join("data.txt");
    std::fs::write(&data, "data").unwrap();
    fixture
        .runtime
        .grant_script(
            "export",
            &ScriptGrantOptions {
                read_roots: vec![data.clone()],
                write_roots: vec![fixture.outbox.clone()],
                ..Default::default()
            },
        )
        .expect("an existing regular read root is granted");
    std::fs::remove_file(&data).unwrap();
    mkfifo(&data);

    let ScriptStepRun::Refused {
        code,
        message,
        record,
    } = RuntimeHost::run_script_step(&fixture.runtime, &request())
    else {
        panic!("a step whose granted read root became a FIFO must refuse");
    };

    assert_eq!(code, script_refusal::ROOT_UNAVAILABLE, "{message}");
    assert!(
        message.contains(&data.display().to_string()),
        "the refusal names the root: {message}"
    );
    assert!(
        record.argv.is_empty() && record.profile.is_none(),
        "refused before the profile was probed or anything spawned: {record:?}"
    );
    assert!(!fixture.ran_marker.exists(), "nothing ran");
}

#[cfg(target_os = "macos")]
#[test]
fn strict_containment_refuses_every_script_step_on_macos() {
    if !super::dispatch_admission::isolated(
        "script_roots::strict_containment_refuses_every_script_step_on_macos",
    ) {
        return;
    }
    let fixture = fixture("[machine]\nworker_containment = \"strict\"\n");
    grant_outbox(&fixture, fixture.outbox.clone()).expect("grant");

    let ScriptStepRun::Refused {
        code,
        message,
        record,
    } = RuntimeHost::run_script_step(&fixture.runtime, &request())
    else {
        panic!("strict containment must refuse a macOS script step");
    };

    assert_eq!(code, script_refusal::SANDBOX_UNAVAILABLE, "{message}");
    assert!(
        message.contains("strict refuses every script step on macOS"),
        "{message}"
    );
    assert!(
        record.argv.is_empty() && record.profile.is_none(),
        "{record:?}"
    );
    assert!(!fixture.ran_marker.exists(), "nothing ran");
}
