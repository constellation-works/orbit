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
