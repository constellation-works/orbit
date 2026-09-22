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
        r#"{"schema_version":1,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","pid":4294967295,"starttime":0}"#,
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
