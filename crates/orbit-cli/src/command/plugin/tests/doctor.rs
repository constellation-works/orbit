use orbit_core::OrbitRuntime;

use super::super::doctor::execute_doctor;
use crate::command::CommandOutput;

#[test]
fn doctor_reports_stale_plugin_callback_records() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let callbacks = runtime.global_root().join("state/plugin-callbacks");
    std::fs::create_dir_all(&callbacks).expect("create callback directory");
    std::fs::write(
        callbacks.join("stale"),
        r#"{"schema_version":2,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","effective_tools":[],"pid":4294967295,"starttime":0}"#,
    )
    .expect("write stale callback record");

    let CommandOutput::Payload(payload) = execute_doctor(&runtime).expect("run plugin doctor")
    else {
        panic!("plugin doctor must return a payload");
    };
    let (document, _) = payload.into_view();
    let finding = document
        .as_array()
        .expect("doctor records")
        .iter()
        .find(|record| record["plugin"] == "plugin callbacks")
        .expect("stale callback finding");
    assert_eq!(finding["status"], "inactive");
    assert!(
        finding["message"]
            .as_str()
            .is_some_and(|message| message.contains("1 stale plugin callback session record")),
        "{finding}"
    );
}

/// A record left behind by a host that predates the effective-tools ceiling
/// is refused for authentication, so no child is still using it. Doctor must
/// still name it, or it is an orphaned mode-0600 file no surface reports
/// [ORB-12879].
#[test]
fn doctor_reports_a_pre_ceiling_callback_record_as_stale() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let callbacks = runtime.global_root().join("state/plugin-callbacks");
    std::fs::create_dir_all(&callbacks).expect("create callback directory");
    // Version 1: no `effective_tools`, and a pid that is live enough to prove
    // the finding does not come from the dead-process check.
    std::fs::write(
        callbacks.join("leftover"),
        format!(
            r#"{{"schema_version":1,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","pid":{},"starttime":0}}"#,
            std::process::id()
        ),
    )
    .expect("write pre-ceiling callback record");

    let CommandOutput::Payload(payload) = execute_doctor(&runtime).expect("run plugin doctor")
    else {
        panic!("plugin doctor must return a payload");
    };
    let (document, _) = payload.into_view();
    let finding = document
        .as_array()
        .expect("doctor records")
        .iter()
        .find(|record| record["plugin"] == "plugin callbacks")
        .expect("pre-ceiling callback finding");
    assert_eq!(finding["status"], "inactive");
    assert!(
        finding["message"]
            .as_str()
            .is_some_and(|message| message.contains("1 stale plugin callback session record")),
        "{finding}"
    );
}

/// The deprecation has to be visible while it is on: a host that still
/// honours the environment token and process ancestry is running one release
/// of compatibility for the credential `setsid` escaped [ORB-12841].
#[test]
fn doctor_reports_the_legacy_callback_identity_deprecation() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let config = runtime.global_root().join("config.toml");
    let mut document = std::fs::read_to_string(&config).unwrap_or_default();
    document.push_str("\n[plugin]\nlegacy_callback_identity = true\n");
    std::fs::write(&config, document).expect("write the host config");

    let CommandOutput::Payload(payload) = execute_doctor(&runtime).expect("run plugin doctor")
    else {
        panic!("plugin doctor must return a payload");
    };
    assert_eq!(payload.exit_code(), 1, "the deprecation needs attention");
    let (document, _) = payload.into_view();
    let finding = document
        .as_array()
        .expect("doctor records")
        .iter()
        .find(|record| {
            record["message"]
                .as_str()
                .is_some_and(|message| message.contains("plugin.legacy_callback_identity"))
        })
        .expect("legacy callback identity finding");
    assert_eq!(finding["plugin"], "plugin callbacks");
    assert!(
        finding["message"]
            .as_str()
            .is_some_and(|message| message.contains("removed in the next release")),
        "{finding}"
    );
}

/// With the key absent, the deprecation is off and doctor stays quiet about
/// it: the default host has nothing to report.
#[test]
fn doctor_is_quiet_about_callback_identity_by_default() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let CommandOutput::Payload(payload) = execute_doctor(&runtime).expect("run plugin doctor")
    else {
        panic!("plugin doctor must return a payload");
    };
    let (document, _) = payload.into_view();
    assert!(
        !document
            .as_array()
            .expect("doctor records")
            .iter()
            .any(|record| record["message"]
                .as_str()
                .is_some_and(|message| message.contains("plugin.legacy_callback_identity"))),
        "{document}"
    );
}

#[test]
fn doctor_reports_an_unparseable_pin_file_and_exits_nonzero() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    std::fs::write(
        runtime.paths().local_dir.join("plugins.yaml"),
        "schemaVersion: 1\nplugins:\n  - name: graph\n    version: invalid\n",
    )
    .expect("write pin file");

    let CommandOutput::Payload(payload) = execute_doctor(&runtime).expect("run plugin doctor")
    else {
        panic!("plugin doctor must return a payload");
    };
    assert_eq!(payload.exit_code(), 1);
    let (document, _) = payload.into_view();
    let finding = document
        .as_array()
        .expect("doctor records")
        .iter()
        .find(|record| record["plugin"] == "pin file")
        .expect("invalid pin file finding");
    assert!(
        finding["message"]
            .as_str()
            .is_some_and(|message| message.contains("invalid")),
        "{finding}"
    );
}
